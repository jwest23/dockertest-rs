//! Represent a concrete instance of an Image, before it is ran as a Container.

use crate::container::PendingContainer;
use crate::image::Image;
use crate::waitfor::{NoWait, WaitFor};
use crate::DockerTestError;

use bollard::{
    container::{
        Config, CreateContainerOptions, HostConfig, InspectContainerOptions, RemoveContainerOptions,
    },
    Docker,
};
use futures::future::TryFutureExt;
use std::collections::HashMap;
use tracing::{event, Level};

/// Specifies the starting policy of a `Composition`.
///
/// A `Strict` policy will enforce that the Composition is started in the order
/// it was added to dockertest. A `Relaxed` policy will not enforce any ordering,
/// all Compositions with a Relaxed policy will be started concurrently.
///
/// All relaxed compositions is asynchronously started before sequentially
/// starting all with a strict `StartPolicy`.
#[derive(Clone, PartialEq)]
pub enum StartPolicy {
    /// Concurrently start the Container with other Relaxed instances.
    Relaxed,
    /// Start Containers' sequentially in the order added to DockerTest.
    Strict,
}

/// Represents an instance of an [Image].
///
/// The `Composition` is used to specialize an Image whose name, version, tag and source is known,
/// but before one can create a [RunningContainer] from an Image, it must be augmented with
/// information about how to start it, how to ensure it has been started, environment variables
/// and runtime commands.
/// Thus, this structure represents the concrete instance of an [Image] that will be started
/// and become a [RunningContainer].
///
/// # Examples
/// ```rust
/// # use dockertest::Composition;
/// let mut hello = Composition::with_repository("hello-world")
///     .with_container_name("my-hello-world")
///     .with_cmd(vec!["command".to_string(), "arg".to_string()]);
/// hello.env("MY_ENV", "MY VALUE");
/// hello.cmd("appended_to_original_cmd!");
/// ```
///
/// [Image]: struct.Image.html
/// [RunningContainer]: struct.RunningContainer.html
#[derive(Clone)]
pub struct Composition {
    /// User provided name of the container.
    /// This will dictate the final container_name and the container_handle_key of the container
    /// that will be created from this Composition.
    user_provided_container_name: Option<String>,

    /// The name of the container to be created by this Composition.
    /// At Composition creation this field defaults to the repository name of the associated image.
    /// When adding Compositions to DockerTest, the container_name will be transformed to the following:
    ///     namespace_of_dockerTest - repository_name - random_generated_suffix
    /// If the user have provided a user_provided_container_name, the container_name will look like
    /// the following:
    ///     namespace_of_dockerTest - user_provided_container_name - random_generated_suffix
    pub(crate) container_name: String,

    /// Trait object that is responsible for
    /// waiting for the container that will
    /// be created to be ready for service.
    /// Defaults to waiting for the container
    /// to appear as running.
    wait: Box<dyn WaitFor>,

    /// The environmentable variables that will be
    /// passed to the container.
    pub(crate) env: HashMap<String, String>,

    /// The command to pass to the container.
    cmd: Vec<String>,

    /// The StartPolicy of this Composition,
    /// defaults to relaxed.
    start_policy: StartPolicy,

    /// The image that this Composition
    /// stems from.
    image: Image,

    /// Volumes associated with this composition, are in the form of: "HOST_PATH/CONTAINER_PATH"
    volumes: Vec<String>,

    /// All user specified container name injections as environment variables.
    /// Tuple contains (handle, env).
    pub(crate) inject_container_name_env: Vec<(String, String)>,
}

impl Composition {
    /// Creates a `Composition` based on the `Image` repository name provided.
    ///
    /// This will internally create the [Image] based on the provided repository name,
    /// and default the tag to `latest`.
    ///
    /// This is the shortcut method of constructing a `Composition`.
    /// See [with_image] to create one with a provided [Image].
    ///
    /// [Image]: struct.Image.html
    /// [with_image]: struct.Composition.html#method.with_image
    pub fn with_repository<T: ToString>(repository: T) -> Composition {
        let copy = repository.to_string();
        Composition {
            user_provided_container_name: None,
            image: Image::with_repository(&copy),
            container_name: copy.replace("/", "-"),
            wait: Box::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
            volumes: Vec::new(),
            inject_container_name_env: Vec::new(),
        }
    }

    /// Creates a `Composition` with the provided `Image`.
    ///
    /// This is the long-winded way of defining a `Composition`.
    /// See [with_repository] to for the shortcut method.
    ///
    /// [with_repository]: struct.Composition.html#method.with_repository
    pub fn with_image(image: Image) -> Composition {
        Composition {
            user_provided_container_name: None,
            container_name: image.repository().to_string().replace("/", "-"),
            image,
            wait: Box::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
            volumes: Vec::new(),
            inject_container_name_env: Vec::new(),
        }
    }

    /// Sets the `StartPolicy` for this `Composition`.
    ///
    /// Defaults to a [relaxed] policy.
    ///
    /// [relaxed]: enum.StartPolicy.html#variant.Relaxed
    pub fn with_start_policy(self, start_policy: StartPolicy) -> Composition {
        Composition {
            start_policy,
            ..self
        }
    }

    /// Assigns the full set of environmental variables available for the [RunningContainer].
    ///
    /// Each key in the map should be the environmental variable name
    /// and its corresponding value will be set as its value.
    ///
    /// This method replaces the entire existing env map provided.
    ///
    /// [RunningContainer]: struct.RunningContainer.html
    pub fn with_env(self, env: HashMap<String, String>) -> Composition {
        Composition { env, ..self }
    }

    /// Sets the command of the container.
    ///
    /// If no entries in the command vector is provided to the [Composition],
    /// the command within the [Image] will be used, if any.
    ///
    /// [Composition]: struct.Composition.html
    /// [Image]: struct.Image.html
    pub fn with_cmd(self, cmd: Vec<String>) -> Composition {
        Composition { cmd, ..self }
    }

    /// Sets the name of the container that will eventually be started.
    ///
    /// This is merely part of the final container name, and the full container name issued
    /// to docker will be generated.
    /// The container name assigned here is also used to resolve the `handle` concept used
    /// by dockertest.
    ///
    /// The container name defaults to the repository name.
    pub fn with_container_name<T: ToString>(self, container_name: T) -> Composition {
        Composition {
            user_provided_container_name: Some(container_name.to_string()),
            ..self
        }
    }

    /// Sets the `WaitFor` trait object for this `Composition`.
    ///
    /// The default `WaitFor` implementation used is [RunningWait].
    ///
    /// [RunningWait]: waitfor/struct.RunningWait.html
    pub fn with_wait_for(self, wait: Box<dyn WaitFor>) -> Composition {
        Composition { wait, ..self }
    }

    /// Sets the environment variable to the given value.
    ///
    /// NOTE: if [with_env] is called after a call to [env], all values added by [env] will be overwritten.
    ///
    /// [env]: struct.Composition.html#method.env
    /// [with_env]: struct.Composition.html#method.with_env
    pub fn env<T: ToString, S: ToString>(&mut self, name: T, value: S) -> &mut Composition {
        self.env.insert(name.to_string(), value.to_string());
        self
    }

    /// Appends the command string to the current command vector.
    ///
    /// If no entries in the command vector is provided to the [Composition],
    /// the command within the [Image] will be used, if any.
    ///
    /// NOTE: if [with_cmd] is called after a call to [cmd], all entries to the command vector
    /// added with [cmd] will be overwritten.
    ///
    /// [cmd]: struct.Composition.html#method.cmd
    /// [with_cmd]: struct.Composition.html#method.with_cmd
    pub fn cmd<T: ToString>(&mut self, cmd: T) -> &mut Composition {
        self.cmd.push(cmd.to_string());
        self
    }

    /// Adds the given volume to the Composition.
    /// The name must match a volume added to the DockerTest instance.
    /// The volume will be mounted into the container on the given path.
    pub fn named_volume<T: ToString, S: ToString>(
        &mut self,
        volume_name: T,
        path_in_container: S,
    ) -> &mut Composition {
        self.volumes.push(format!(
            "{}:{}",
            volume_name.to_string(),
            path_in_container.to_string()
        ));
        self
    }

    /// Inject the generated container name identified by `handle` into
    /// this Composition environment variable `env`.
    ///
    /// This is used to establish inter communication between running containers
    /// controlled by dockertest. This is traditionally established through environment variables
    /// for connection details, and thus the DNS resolving capabilities within docker will
    /// map the container name into the correct IP address.
    ///
    /// To correctly use this feature, the `StartPolicy` between the dependant containers
    /// must be configured such that these connections can successfully be established.
    /// Dockertest will not make any attempt to verify the integrity of these dependencies.
    pub fn inject_container_name<T: ToString, E: ToString>(
        &mut self,
        handle: T,
        env: E,
    ) -> &mut Composition {
        self.inject_container_name_env
            .push((handle.to_string(), env.to_string()));
        self
    }

    // Configure the container's name with the given namespace as prefix
    // and suffix.
    // We do this to ensure that we do not have overlapping container names
    // and make it clear which containers are run by DockerTest.
    pub(crate) fn configure_container_name(&mut self, namespace: &str, suffix: &str) {
        let name = match &self.user_provided_container_name {
            None => self.image.repository(),
            Some(n) => n,
        };

        // The docker daemon does not like '/' or '\' in container names
        let stripped_name = name.replace("/", "_");

        self.container_name = format!("{}-{}-{}", namespace, stripped_name, suffix);
    }

    // Consumes the Composition, creates the container and returns the Container object if it
    // was succesfully created.
    pub(crate) async fn create(
        self,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        event!(Level::INFO, "creating container: {}", self.container_name);

        let start_policy_clone = self.start_policy.clone();
        let container_name_clone = self.container_name.clone();

        // Ensure we can remove the previous container instance, if it somehow still exists.
        // Only bail on non-recoverable failure.
        match remove_container_if_exists(client, &self.container_name).await {
            Ok(_) => {}
            Err(e) => match e {
                DockerTestError::Recoverable(_) => {}
                _ => return Err(e),
            },
        }

        let image_id = self.image.retrieved_id();
        // Additional programming guard.
        // This Composition cannot be created without an image id, which
        // is set through `Image::pull`
        if image_id.is_empty() {
            return Err(DockerTestError::Processing("`Composition::create()` invoked without populatting its image through `Image::pull()`".to_string()));
        }

        // As we can't return temporary values owned by this closure
        // we have to first convert our map into a vector of owned strings,
        // then convert it to a vector of borrowed strings (&str).
        // There is probably a better way to do this...
        let envs: Vec<String> = self
            .env
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect();
        let envs = envs.iter().map(|s| s.as_ref()).collect();
        let cmds = self.cmd.iter().map(|s| s.as_ref()).collect();
        let mut volumes: HashMap<&str, HashMap<(), ()>> = HashMap::new();
        for v in self.volumes.iter() {
            volumes.insert(v.as_str(), HashMap::new());
        }

        // Construct host config
        let host_config = network.map(|n| HostConfig {
            network_mode: Some(n),
            ..Default::default()
        });

        // Construct options for create container
        let options = Some(CreateContainerOptions {
            name: &self.container_name,
        });
        let config = Config::<&str> {
            image: Some(&image_id),
            cmd: Some(cmds),
            env: Some(envs),
            volumes: Some(volumes),
            host_config,
            ..Default::default()
        };

        let container_info = client
            .create_container(options, config)
            .map_err(|e| DockerTestError::Daemon(format!("failed to create container: {}", e)))
            .await?;

        Ok(PendingContainer::new(
            &container_name_clone,
            &container_info.id,
            self.handle(),
            start_policy_clone,
            self.wait,
            client.clone(),
        ))
    }

    // Returns the Image associated with this Composition.
    pub(crate) fn image(&self) -> &Image {
        &self.image
    }

    /// Retrieve a copy of the applicable handle name for this Composition.
    pub fn handle(&self) -> String {
        match &self.user_provided_container_name {
            None => self.image.repository().to_string(),
            Some(n) => n.clone(),
        }
    }
}

// Forcefully removes the given container if it exists.
async fn remove_container_if_exists(client: &Docker, name: &str) -> Result<(), DockerTestError> {
    client
        .inspect_container(name, None::<InspectContainerOptions>)
        .map_err(|e| DockerTestError::Recoverable(format!("container did not exist: {}", e)))
        .await?;

    // We were able to inspect it successfully, it exists.
    // Therefore, we can simply force remove it.
    let options = Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
    });
    client
        .remove_container(name, options)
        .map_err(|e| DockerTestError::Daemon(format!("failed to remove existing container: {}", e)))
        .await
}

#[cfg(test)]
mod tests {
    use crate::composition::{remove_container_if_exists, Composition, StartPolicy};
    use crate::image::{Image, Source};
    use crate::DockerTestError;

    use bollard::Docker;
    use std::collections::HashMap;

    // Tests that the with_repository constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_repository_constructor() {
        let repository = "this_is_a_repository".to_string();

        let instance = Composition::with_repository(&repository);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = match instance.start_policy {
            StartPolicy::Relaxed => true,
            _ => false,
        };
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests that the with_image constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_image_constructor() {
        let repository = "this_is_a_repository".to_string();
        let image = Image::with_repository(&repository);

        let instance = Composition::with_image(image);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = match instance.start_policy {
            StartPolicy::Relaxed => true,
            _ => false,
        };
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests all methods that consumes the Composition
    // and mutates a field
    #[test]
    fn test_mutators() {
        let mut env = HashMap::new();

        let env_variable = "GOPATH".to_string();
        let env_value = "/home/kim/unsafe".to_string();

        env.insert(env_variable, env_value);
        let expected_env = env.clone();

        let cmd = "this_is_a_command".to_string();
        let mut cmds = Vec::new();
        cmds.push(cmd);

        let expected_cmds = cmds.clone();

        let repository = "this_is_a_repository".to_string();

        let container_name = "this_is_a_container_name";

        let instance = Composition::with_repository(&repository)
            .with_start_policy(StartPolicy::Strict)
            .with_env(env)
            .with_cmd(cmds)
            .with_container_name(container_name);

        let equal = match instance.start_policy {
            StartPolicy::Strict => true,
            _ => false,
        };

        assert!(equal, "start_policy was not changed after invoking mutator");
        assert_eq!(
            expected_env, instance.env,
            "environmental variables not set correctly"
        );

        assert_eq!(expected_cmds, instance.cmd, "commands not set correctly");

        let correct_container_name = match instance.user_provided_container_name {
            Some(n) => n == container_name,
            None => false,
        };

        assert!(correct_container_name, "container_name not set correctly");
    }

    // Tests that the env method succesfully
    // adds the given environment variable to the Composition
    #[test]
    fn test_add_env() {
        let env_variable = "this_is_an_env_var".to_string();
        let env_value = "this_is_an_env_value".to_string();

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(&repository);

        instance.env(env_variable.clone(), env_value.clone());

        assert_eq!(
            *instance
                .env
                .get(&env_variable)
                .expect("failed to get value from map that should be there"),
            env_value,
            "environmental variable not added correctly"
        );
    }

    // Tests that the cmd method succesfully
    // adds the given command to the Composition
    #[test]
    fn test_add_cmd() {
        let cmd = "this_is_a_command".to_string();
        let expected_cmd = vec![cmd.clone()];

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(&repository);

        instance.cmd(cmd);

        assert_eq!(
            instance.cmd, expected_cmd,
            "command value not added correctly"
        );
    }

    /// Tests that we cannot create a container from a non-existent local repository image.
    #[tokio::test]
    async fn test_create_with_non_existing_local_image() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = "dockertest_create_with_non_existing_local_image";
        let composition = Composition::with_repository(repository);

        // Invoking image pull to populate the image id should err
        // TODO: assert a proper error message
        assert!(composition
            .image
            .pull(&client, &Source::Local)
            .await
            .is_err());

        // This will then fail due to missing image id
        let result = composition.create(&client, None).await;
        // TODO: assert a proper error message
        assert!(
            result.is_err(),
            "should fail to start a Composition with non-exisiting image"
        );
    }

    /// Check that a simple composition from repository can be successfully created.
    #[tokio::test]
    async fn test_simple_create_composition_from_repository_success() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = "dockertest-rs/hello";
        let mut composition = Composition::with_repository(repository);
        composition.container_name = "test_simple_create_composition_from_repository_success".to_string();

        // ensure image metadata is populated (through pull infrastructure)
        composition
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        let result = composition.create(&client, None).await;
        assert!(
            result.is_ok(),
            format!("failed to start Composition: {}", result.err().unwrap())
        );
    }

    /// Tests that two consecutive Compositions creating a container with the exact same
    /// `container_name` will still be allowed to be created.
    ///
    /// The expected behaviour is thus to remove the old container
    /// (since we assume it will be an _old_ name collision).
    #[tokio::test]
    async fn test_create_with_existing_container() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = "dockertest-rs/hello";
        let container_name = "dockertest_create_with_existing_container".to_string();

        let mut composition1 = Composition::with_repository(repository);
        // configure the `container_name` inline instead of through `with_container_name`,
        // to avoid the user provided container name logic.
        composition1.container_name = container_name;
        // ensure image metadata is populated (through pull infrastructure)
        composition1
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        let composition2 = composition1.clone();

        // Initial setup - first container that already exists.
        let result = composition1.create(&client, None).await;
        assert!(
            result.is_ok(),
            format!(
                "failed to start first composition: {}",
                result.err().unwrap()
            )
        );

        // Creating a second one should still be allowed, since we expect the first one
        // to be removed.
        let result = composition2.create(&client, None).await;
        assert!(
            result.is_ok(),
            format!(
                "failed to start second composition: {}",
                result.err().unwrap()
            )
        );
    }

    /// Tests the `remove_container_if_exists` method when container exists.
    #[tokio::test]
    async fn test_remove_existing_container() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = "dockertest-rs/hello";
        let container_name = "dockertest_remove_existing_container_test_name";
        let mut composition = Composition::with_repository(repository);
        composition.container_name = container_name.to_string();

        // ensure image metadata is populated (through pull infrastructure)
        composition
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        // Create out composition
        let result = composition.create(&client, None).await;
        assert!(
            result.is_ok(),
            format!("failed to start composition: {}", result.err().unwrap())
        );

        // Remove it by name
        let result = remove_container_if_exists(&client, container_name).await;
        assert!(
            result.is_ok(),
            format!(
                "failed to remove existing container: {}",
                result.unwrap_err()
            )
        );
    }

    /// Tests that we fail when trying to remove a non-existing container through
    /// `remove_container_if_exists`.
    #[tokio::test]
    async fn test_remove_non_existing_container() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");

        let result = remove_container_if_exists(&client, "dockertest_non_existing_container").await;

        let res = match result {
            Ok(_) => false,
            Err(e) => match e {
                DockerTestError::Recoverable(_) => true,
                _ => false,
            },
        };
        assert!(res, format!("should fail to remove non-existing container"));
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has not specified a container_name
    #[test]
    fn test_configurate_container_name_without_user_supplied_name() {
        let repository = "hello-world";
        let mut composition = Composition::with_repository(&repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, repository, suffix);

        composition.configure_container_name(&namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has specified a container_name
    #[test]
    fn test_configurate_container_name_with_user_supplied_name() {
        let repository = "hello-world";
        let container_name = "this_is_a_container";
        let mut composition =
            Composition::with_repository(&repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, container_name, suffix);

        composition.configure_container_name(&namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when a user provided name is given.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_with_user_supplied_name_containing_slashes() {
        let repository = "hello-world";
        let container_name = "this/is/a_container";
        let expected_container_name = "this_is_a_container";

        let mut composition =
            Composition::with_repository(&repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        composition.configure_container_name(&namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when no user provided container name is provided.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_without_user_supplied_name_containing_slashes() {
        let repository = "hello/world";
        let expected_container_name = "hello_world";

        let mut composition = Composition::with_repository(&repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        composition.configure_container_name(&namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }
}

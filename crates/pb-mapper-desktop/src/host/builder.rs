//! How a host is configured and started.

use super::Host;
use crate::{
    Backend, BackendCaps, Result,
    launch::{Launch, Launcher},
    platform,
    security::HostIdentity,
};
use pb_mapper_client::sdk::{Client, RegisterOptions};
use std::{marker::PhantomData, path::PathBuf, sync::Arc};

/// Which display a host shares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DisplaySelection {
    /// The primary display.
    #[default]
    Primary,
    /// The display of this [`Display::id`]. The id belongs to the running
    /// system: a reboot or a mode change may give the display another one.
    ///
    /// [`Display::id`]: crate::Display::id
    Id(u32),
    /// The display of this [`Display::name`] (`\\.\DISPLAY1` on Windows),
    /// which stays the same: the key to keep when a choice is saved.
    ///
    /// [`Display::name`]: crate::Display::name
    Device(String),
}

/// How a host reaches the desktop it shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Access {
    /// This process's own desktop, as its user.
    #[default]
    Ordinary,
    /// The installed, authorized privileged helper, which also reaches the
    /// secure desktop (UAC and the lock screen). Windows only.
    Privileged,
}

/// What a host shares and how. Build it with [`HostOptions::new`], which
/// takes every path default from [`RegisterOptions::default`]: a viewer on the
/// same local network first, then a direct path, then the relay.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HostOptions {
    /// The registered service: `desktop:` and up to 128 ASCII letters,
    /// digits, `.`, `_`, `:` or `-`.
    pub service: String,
    pub display: DisplaySelection,
    pub access: Access,
    /// Whether a viewer may take control. Off by default: viewing only.
    pub allow_control: bool,
    /// The registration's [`RegisterOptions::allow_direct`].
    pub allow_direct: bool,
    /// The registration's [`RegisterOptions::lan_first`]: probe a viewer's
    /// addresses on a shared local network first. Which path a session
    /// takes is the viewer's choice, under its own `ConnectOptions`.
    pub lan_first: bool,
    /// The registration's [`RegisterOptions::lan_tcp`]: serve a viewer on the
    /// same local network over TCP when it asks.
    pub lan_tcp: bool,
    /// Where an in-process host keeps what it must restore after a crash;
    /// `None` is the platform's own directory. A privileged host never uses
    /// it: the helper keeps its own, protected.
    pub state_dir: Option<PathBuf>,
}
impl HostOptions {
    pub fn new(service: impl Into<String>) -> Self {
        let register = RegisterOptions::default();
        Self {
            service: service.into(),
            display: DisplaySelection::default(),
            access: Access::default(),
            allow_control: false,
            allow_direct: register.allow_direct,
            lan_first: register.lan_first,
            lan_tcp: register.lan_tcp,
            state_dir: None,
        }
    }
}

/// The platform's own capture and input, through [`HostOptions::access`].
pub struct Native;
/// An embedder's backends, each built by the factory on its session's own
/// thread.
pub struct Custom<B, F> {
    factory: F,
    _backend: PhantomData<fn() -> B>,
}
/// What a host starts from once its source is known: the platform's own
/// (`native`, which a restart builds again for its new options), an
/// embedder's, or one the crate built itself (the background host's helper
/// client).
pub(crate) struct Launched {
    pub(super) launch: Arc<dyn Launch>,
    pub(super) native: bool,
}

/// How a [`Host`] is started: [`Host::builder`], then any of `identity`,
/// `admission` and `backend`, then `start`.
pub struct HostBuilder<S = Native> {
    pub(super) options: HostOptions,
    pub(super) identity: Option<HostIdentity>,
    pub(super) admission: Option<Arc<dyn crate::ControlAdmission>>,
    pub(super) source: S,
}
impl<S> HostBuilder<S> {
    /// Resume an explicitly authorized grant: the host keeps this identity,
    /// and so the invitation a viewer saved. Without one it generates a new
    /// identity. Revoke durable access by discarding the saved identity.
    pub fn identity(mut self, identity: HostIdentity) -> Self {
        self.identity = Some(identity);
        self
    }
    /// Coordinate control with the embedding application's other input
    /// writers.
    pub fn admission(mut self, admission: Arc<dyn crate::ControlAdmission>) -> Self {
        self.admission = Some(admission);
        self
    }
    /// Capture and inject with the embedder's backends instead of the
    /// platform's.
    pub fn backend<B, F>(self, factory: F) -> HostBuilder<Custom<B, F>>
    where
        B: Backend,
        F: Fn() -> Result<B> + Send + Sync + 'static,
    {
        self.source(Custom {
            factory,
            _backend: PhantomData,
        })
    }
    pub(crate) fn launch(self, launch: Arc<dyn Launch>) -> HostBuilder<Launched> {
        self.source(Launched {
            launch,
            native: false,
        })
    }
    fn source<T>(self, source: T) -> HostBuilder<T> {
        HostBuilder {
            options: self.options,
            identity: self.identity,
            admission: self.admission,
            source,
        }
    }
}
impl HostBuilder<Native> {
    /// Refused before registering where this platform cannot host, or the
    /// privileged helper is not ready.
    pub async fn start(self, client: &Client) -> Result<Host> {
        let launch = platform::native_launch(&self.options).await?;
        let mut native = self.launch(launch);
        native.source.native = true;
        native.start(client).await
    }
}
impl<B, F> HostBuilder<Custom<B, F>>
where
    B: Backend,
    F: Fn() -> Result<B> + Send + Sync + 'static,
{
    pub async fn start(self, client: &Client) -> Result<Host> {
        let HostBuilder {
            options,
            identity,
            admission,
            source,
        } = self;
        HostBuilder {
            options,
            identity,
            admission,
            source: Launched {
                launch: Arc::new(Launcher::new(source.factory, BackendCaps::default())),
                native: false,
            },
        }
        .start(client)
        .await
    }
}
impl HostBuilder<Launched> {
    pub(crate) async fn start(self, client: &Client) -> Result<Host> {
        Host::launch(client, self).await
    }
}

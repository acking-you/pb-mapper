//! How a host starts each admitted session's thread C on its backend. One
//! `Launch::prepare` call per session is the only dynamic dispatch on a host's
//! session path; behind it thread C runs on a concrete backend type.

use crate::{
    Backend, BackendCaps, Result,
    worker::{self, Prepared},
};
use std::{marker::PhantomData, sync::Arc};

/// Puts back what this launcher's sessions left changed and could not
/// restore; whether something is still left.
pub(crate) type Recovery = Arc<dyn Fn() -> Result<bool> + Send + Sync>;

pub(crate) trait Launch: Send + Sync + 'static {
    /// Start thread C on a new backend, asking it about H.265 when
    /// `probe_h265`.
    fn prepare(&self, probe_h265: bool) -> Prepared;
    /// What sessions of this launcher can do when a backend does not
    /// describe itself in time.
    fn fallback_caps(&self) -> BackendCaps;
    /// The display-mode journal's replay, which the host retries while no
    /// session is live; `None` where sessions journal nothing.
    fn recovery(&self) -> Option<Recovery>;
}

/// Sessions on the backends `factory` builds, each on its own thread C.
pub(crate) struct Launcher<B, F> {
    factory: Arc<F>,
    fallback: BackendCaps,
    recovery: Option<Recovery>,
    _backend: PhantomData<fn() -> B>,
}
impl<B, F> Launcher<B, F> {
    pub(crate) fn new(factory: F, fallback: BackendCaps) -> Self {
        Self {
            factory: Arc::new(factory),
            fallback,
            recovery: None,
            _backend: PhantomData,
        }
    }
    /// Its sessions journal display-mode changes, which `recovery` replays.
    pub(crate) fn recovering(
        mut self,
        recovery: impl Fn() -> Result<bool> + Send + Sync + 'static,
    ) -> Self {
        self.recovery = Some(Arc::new(recovery));
        self
    }
}
impl<B, F> Launch for Launcher<B, F>
where
    B: Backend,
    F: Fn() -> Result<B> + Send + Sync + 'static,
{
    fn prepare(&self, probe_h265: bool) -> Prepared {
        let factory = self.factory.clone();
        worker::prepare(move || factory(), probe_h265)
    }
    fn fallback_caps(&self) -> BackendCaps {
        self.fallback.clone()
    }
    fn recovery(&self) -> Option<Recovery> {
        self.recovery.clone()
    }
}

//! How a host starts each admitted session's thread C on its backend. One
//! `Launch::prepare` call per session is the only dynamic dispatch on a host's
//! session path; behind it thread C runs on a concrete backend type.

use crate::{
    Backend, BackendCaps, Result,
    worker::{self, Prepared},
};
use std::{marker::PhantomData, sync::Arc};

pub(crate) trait Launch: Send + Sync + 'static {
    /// Start thread C on a new backend, asking it about H.265 when
    /// `probe_h265`.
    fn prepare(&self, probe_h265: bool) -> Prepared;
    /// What sessions of this launcher can do when a backend does not
    /// describe itself in time.
    fn fallback_caps(&self) -> BackendCaps;
}

/// Sessions on the backends `factory` builds, each on its own thread C.
pub(crate) struct Launcher<B, F> {
    factory: Arc<F>,
    fallback: BackendCaps,
    _backend: PhantomData<fn() -> B>,
}
impl<B, F> Launcher<B, F> {
    pub(crate) fn new(factory: F, fallback: BackendCaps) -> Self {
        Self {
            factory: Arc::new(factory),
            fallback,
            _backend: PhantomData,
        }
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
}

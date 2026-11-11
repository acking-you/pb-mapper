//! Hand a connect tunnel a listener the case already bound, through the
//! listener-provider seam of the production subscriber loop: the case owns
//! its socket throughout setup, with no port reserved and dropped first.
//! `regression_client` includes this file by path.

use std::cell::RefCell;

use tokio::net::TcpListener;

tokio::task_local! {
    static BOUND_LISTENER: RefCell<Option<TcpListener>>;
}

/// Run `tunnel`, a `run_client_side_cli*::<BoundListener, _>` future, with
/// `listener` as the listener its first bind returns. A second bind, which
/// the loop makes only after its listener failed, fails in turn.
pub(crate) async fn with_bound_listener<F: Future>(listener: TcpListener, tunnel: F) -> F::Output {
    BOUND_LISTENER
        .scope(RefCell::new(Some(listener)), tunnel)
        .await
}

/// The `ListenerProvider` whose bind returns the listener in scope.
pub(crate) struct BoundListener;
pub(crate) struct Bound(TcpListener);

impl uni_stream::stream::ListenerProvider for BoundListener {
    type Listener = Bound;

    async fn bind<A: uni_stream::addr::ToSocketAddrs + Send>(
        _addr: A,
    ) -> std::io::Result<Self::Listener> {
        BOUND_LISTENER.with(|listener| {
            listener
                .borrow_mut()
                .take()
                .map(Bound)
                .ok_or_else(|| std::io::Error::other("the bound listener was already taken"))
        })
    }
}

impl uni_stream::stream::StreamAccept for Bound {
    type Item = uni_stream::stream::TcpStreamImpl;

    async fn accept(&self) -> std::io::Result<(Self::Item, std::net::SocketAddr)> {
        let (stream, addr) = self.0.accept().await?;
        Ok((uni_stream::stream::TcpStreamImpl::new(stream), addr))
    }
}

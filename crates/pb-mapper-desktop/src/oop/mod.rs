//! The out-of-process backend: a [`Backend`](crate::Backend) carried over a
//! byte transport to a worker that serves its own backend. The protocol
//! (`proto`), framing (`ipc`), client (`RemoteBackend`) and server (`serve`)
//! are neutral; a platform brings the transport, peer verification,
//! installation and session tracking. Windows serves its privileged helper
//! over a named pipe.
pub(crate) mod client;
pub(crate) mod ipc;
pub(crate) mod proto;
// Only the helper's worker serves, in the helper binary.
#[cfg_attr(not(feature = "windows-helper"), allow(dead_code))]
pub(crate) mod server;

/// What a backend's calls cross: a pipe, a socket, or a test's duplex.
pub(crate) trait Transport:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> Transport for T {}

//! The host's listener: each connection's handshake and session.

use super::{BACKEND_ANSWER_WAIT, Live, Shared};
use crate::{
    Display, QualityPreset, Result, VideoCodec, VideoLimits, VideoSize,
    launch::Launch,
    protocol::{
        CAPABILITY_DISPLAY_MODE, CAPABILITY_H265, HostMessage, RejectReason, ViewerMessage,
        valid_capabilities,
    },
    qos::scaled_size,
    session::{self, Session},
    wire,
    worker::{self, WorkerConfig},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

/// The TLS handshake and the viewer's `Authenticate` must arrive within this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// A `Rejected` and the close after it.
const REJECT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// Connections in their handshake or session at once; one holds the viewer slot.
const MAX_CONNECTIONS: usize = 8;

/// What the viewer asked for in a valid `Authenticate`.
struct Request {
    /// One or two, never `h265`.
    codecs: Vec<VideoCodec>,
    limits: VideoLimits,
    quality: QualityPreset,
    /// What the viewer listed; values this host does not know are ignored.
    capabilities: Vec<String>,
}

/// A viewer that holds the slot and heard `Accepted`, with its thread C
/// waiting for the session.
struct Admitted {
    stream: TlsStream<TcpStream>,
    permit: OwnedSemaphorePermit,
    request: Request,
    codec: VideoCodec,
    /// The size the encoder is expected to produce.
    video: VideoSize,
    /// What `Accepted` listed.
    capabilities: Vec<String>,
    prepared: worker::Prepared,
}

/// A session's codec: H.265 when the viewer decodes it and the backend
/// encodes it (`h265`), else as every v2 host chooses: H.264 when listed,
/// else JPEG.
fn choose_codec(codecs: &[VideoCodec], h265: bool) -> VideoCodec {
    if h265 {
        VideoCodec::H265
    } else if codecs.contains(&VideoCodec::H264) {
        VideoCodec::H264
    } else {
        VideoCodec::Jpeg
    }
}

/// The size the encoder makes of `display` for `limits`, or the bounds
/// themselves when the display is not known.
fn expected_video(display: Option<&Display>, limits: VideoLimits) -> VideoSize {
    let (width, height) = display.map_or((limits.max_width, limits.max_height), |display| {
        scaled_size(
            display.width,
            display.height,
            limits.max_width,
            limits.max_height,
        )
    });
    VideoSize { width, height }
}

/// Frees the viewer slot however `serve` ends, a panic or a dropped task
/// included: the host's status forgets this session, and the display-mode
/// retry looks, since thread C put the display back as the session ended
/// unless it could not.
struct SlotRelease<'a> {
    shared: &'a Shared,
    session: Arc<Session>,
}
impl Drop for SlotRelease<'_> {
    fn drop(&mut self) {
        let mut slot = self.shared.session.lock();
        if slot
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(&held.session, &self.session))
        {
            slot.take();
        }
        drop(slot);
        self.shared.restore_pending.store(true, Ordering::Release);
        self.shared.restore.notify_one();
    }
}

/// The listener's side of the host: every connection's handshake and session.
pub(super) struct Acceptor {
    pub(super) tls: TlsAcceptor,
    pub(super) token: String,
    pub(super) launch: Arc<dyn Launch>,
    pub(super) admission: Option<Arc<dyn crate::ControlAdmission>>,
    pub(super) shared: Arc<Shared>,
    /// The single viewer slot.
    pub(super) slot: Arc<Semaphore>,
    pub(super) cancel: CancellationToken,
}
impl Acceptor {
    pub(super) async fn accept(self: Arc<Self>, listener: TcpListener) {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => break,
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                    let Ok((socket, _)) = accepted else { break };
                    connections.spawn(self.clone().serve(socket));
                }
            }
        }
        // Each session sees the cancel and ends with `Stopped`.
        while connections.join_next().await.is_some() {}
    }

    async fn serve(self: Arc<Self>, socket: TcpStream) {
        let accepted = tokio::select! {
            () = self.cancel.cancelled() => return,
            accepted = self.accept_session(socket) => accepted,
        };
        let Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            capabilities,
            prepared,
        }) = accepted
        else {
            return;
        };
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_host_accepted",
            codec = ?codec,
            max_width = request.limits.max_width,
            max_height = request.limits.max_height,
            max_fps = request.limits.max_fps,
            width = video.width,
            height = video.height,
            quality = ?request.quality,
        );
        let session = Session::new(
            self.shared.policy.clone(),
            self.cancel.child_token(),
            codec,
            request.limits,
            request.quality,
            &request.capabilities,
            capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_DISPLAY_MODE),
        );
        session.expect_video_size(video);
        *self.shared.session.lock() = Some(Live {
            session: session.clone(),
            capabilities,
        });
        let slot = SlotRelease {
            shared: &self.shared,
            session: session.clone(),
        };
        let mut worker = prepared.start(
            session.shared.clone(),
            WorkerConfig {
                codec,
                limits: request.limits,
                nominal_fps: request.limits.max_fps,
                admission: self.admission.clone(),
            },
        );
        session::run(
            session.clone(),
            stream,
            async move { worker.exited().await },
        )
        .await;
        // The slot frees before the permit, which a panic keeps too: `permit`
        // was bound first, so it is dropped last.
        drop(slot);
        drop(permit);
    }

    /// The handshake, then `Accepted` with the session's codec and
    /// capabilities. Thread C starts building the backend as soon as the
    /// viewer holds the slot, and `Accepted` waits until the backend described
    /// itself: which display it captures, which sizes the first IDR's budget;
    /// what it can do; and, for a viewer that decodes H.265, whether it
    /// encodes H.265, unless an H.265 encoder already failed in this process.
    async fn accept_session(&self, socket: TcpStream) -> Option<Admitted> {
        let (mut stream, permit, request) = self.handshake(socket).await?;
        let offered = request
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_H265);
        // Never H.265 again in this process once one of its encoders failed,
        // whatever a backend says.
        let probe = offered && !worker::h265_failed();
        let mut prepared = self.launch.prepare(probe);
        let deadline = tokio::time::Instant::now() + BACKEND_ANSWER_WAIT;
        let description = tokio::time::timeout_at(deadline, &mut prepared.description)
            .await
            .ok()
            .and_then(|answer| answer.ok())
            .and_then(Result::ok);
        let video = expected_video(
            description.as_ref().map(|described| &described.display),
            request.limits,
        );
        let caps =
            description.map_or_else(|| self.launch.fallback_caps(), |described| described.caps);
        let h265 = match prepared.h265.as_mut() {
            Some(answer) => matches!(
                tokio::time::timeout_at(deadline, answer).await,
                Ok(Ok(true))
            ),
            None => false,
        };
        let codec = choose_codec(&request.codecs, probe && h265);
        let capabilities = caps.values();
        let accepted = HostMessage::Accepted {
            codec,
            limits: request.limits,
            nominal_fps: request.limits.max_fps,
            quality: request.quality,
            capabilities: capabilities.clone(),
        };
        wire::write_message(&mut stream, &accepted, None)
            .await
            .ok()?;
        Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            capabilities,
            prepared,
        })
    }

    /// TLS and `Authenticate`; `Rejected` when it fails. The token is checked
    /// first, so `busy` never tells a stranger that someone is connected.
    async fn handshake(
        &self,
        socket: TcpStream,
    ) -> Option<(TlsStream<TcpStream>, OwnedSemaphorePermit, Request)> {
        // State and small video units must not wait for Nagle/delayed-ACK
        // coalescing on the publisher's accepted loopback connection.
        socket.set_nodelay(true).ok()?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let mut stream = tokio::time::timeout_at(deadline, self.tls.accept(socket))
            .await
            .ok()?
            .ok()?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(ViewerMessage::Authenticate {
            token,
            codecs,
            limits,
            quality,
            capabilities,
        })) = wire::read_header(&mut stream, remaining).await
        else {
            return None;
        };
        let rejected = if !self.token_matches(&token) {
            RejectReason::Denied
        } else if !(1..=2).contains(&codecs.len())
            // H.265 is the `h265` capability, never a codec offered.
            || codecs.contains(&VideoCodec::H265)
            || limits.validate().is_err()
            || !valid_capabilities(&capabilities)
        {
            RejectReason::InvalidRequest
        } else if let Ok(permit) = self.slot.clone().try_acquire_owned() {
            return Some((
                stream,
                permit,
                Request {
                    codecs,
                    limits,
                    quality,
                    capabilities,
                },
            ));
        } else {
            RejectReason::Busy
        };
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_host_rejected",
            reason = ?rejected,
        );
        let _ = tokio::time::timeout(REJECT_WRITE_TIMEOUT, async {
            let rejected = HostMessage::Rejected { reason: rejected };
            wire::write_message(&mut stream, &rejected, None).await?;
            stream.shutdown().await
        })
        .await;
        None
    }

    /// A constant-time comparison, over digests so the length leaks nothing.
    fn token_matches(&self, presented: &str) -> bool {
        use ring::digest::{SHA256, digest};
        use subtle::ConstantTimeEq;
        bool::from(
            digest(&SHA256, presented.as_bytes())
                .as_ref()
                .ct_eq(digest(&SHA256, self.token.as_bytes()).as_ref()),
        )
    }
}

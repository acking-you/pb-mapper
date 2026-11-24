//! The host's listener: each connection's handshake and session.

use super::{BACKEND_ANSWER_WAIT, Room, Shared};
use crate::{
    Display, QualityPreset, Result, VideoCodec, VideoLimits, VideoSize,
    launch::Launch,
    protocol::{
        CAPABILITY_ARRIVAL_TIME, CAPABILITY_H265, CAPABILITY_KEYFRAME_SEQ, CAPABILITY_PEER_CURSORS,
        HostMessage, PeerColor, PeerSlot, RejectReason, StopReason, ViewerMessage,
        valid_capabilities,
    },
    qos::scaled_size,
    session::{self, Session, stop_message},
    wire,
    worker::{self, RoomShared, RoomSlotConfig},
};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;

/// The TLS handshake and the viewer's `Authenticate` must arrive within this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// A `Rejected` and the close after it.
const REJECT_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// Connections in their handshake or session at once; up to `MAX_VIEWERS` hold viewer slots.
const MAX_CONNECTIONS: usize = 12;

/// What the viewer asked for in a valid `Authenticate`.
struct Request {
    /// One or two, never `h265`.
    codecs: Vec<VideoCodec>,
    limits: VideoLimits,
    quality: QualityPreset,
    /// What the viewer listed; values this host does not know are ignored.
    capabilities: Vec<String>,
}

/// A viewer that holds a room slot and heard `Accepted`.
struct Admitted {
    stream: TlsStream<TcpStream>,
    permit: OwnedSemaphorePermit,
    request: Request,
    codec: VideoCodec,
    /// The size the encoder is expected to produce.
    video: VideoSize,
    room: Arc<Room>,
    slot: PeerSlot,
    conn_id: u64,
    session: Arc<Session>,
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

/// What every host speaks, whatever its backend: each is listed back to a
/// viewer that listed it.
const SESSION_CAPABILITIES: [&str; 3] = [
    CAPABILITY_PEER_CURSORS,
    CAPABILITY_ARRIVAL_TIME,
    CAPABILITY_KEYFRAME_SEQ,
];

/// `Accepted.capabilities`: the backend's, then each of
/// `SESSION_CAPABILITIES` the viewer listed.
fn negotiated_capabilities(host_caps: &[String], requested: &[String]) -> Vec<String> {
    let mut capabilities = host_caps.to_vec();
    for wanted in SESSION_CAPABILITIES {
        if requested.iter().any(|capability| capability == wanted)
            && !capabilities.iter().any(|capability| capability == wanted)
        {
            capabilities.push(wanted.to_owned());
        }
    }
    capabilities
}

async fn send_rejected(mut stream: TlsStream<TcpStream>, rejected: RejectReason) {
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
}

/// Frees the viewer slot however `serve` ends, a panic or a dropped task
/// included: the host's status forgets this session, and says so, and the
/// display-mode retry looks, since thread C put the display back as the
/// session ended unless it could not.
struct SlotRelease<'a> {
    shared: &'a Shared,
    room: Arc<Room>,
    slot: PeerSlot,
    conn_id: u64,
    armed: bool,
}
impl Drop for SlotRelease<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.room.detach(self.slot, self.conn_id) {
            self.room.shared.cancel.cancel();
            self.room.shared.wake.notify();
            self.shared.clear_room(&self.room);
            self.shared.restore_pending.store(true, Ordering::Release);
            self.shared.restore.notify_one();
        }
        self.shared.changes.bump();
    }
}

/// The listener's side of the host: every connection's handshake and session.
pub(super) struct Acceptor {
    pub(super) tls: TlsAcceptor,
    pub(super) token: String,
    pub(super) launch: Arc<dyn Launch>,
    pub(super) admission: Option<Arc<dyn crate::ControlAdmission>>,
    pub(super) shared: Arc<Shared>,
    pub(super) room: AsyncMutex<Option<Arc<Room>>>,
    /// Up to `MAX_VIEWERS` concurrent viewer slots.
    pub(super) slot: Arc<Semaphore>,
    pub(super) cancel: CancellationToken,
}
impl Acceptor {
    pub(super) async fn accept(self: Arc<Self>, listener: TcpListener) {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                biased;
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
            biased;
            () = self.cancel.cancelled() => return,
            accepted = self.accept_session(socket) => accepted,
        };
        let Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            room,
            slot,
            conn_id,
            session,
        }) = accepted
        else {
            return;
        };
        tracing::info!(
            target: "pb_mapper_desktop::session",
            event = "desktop_host_accepted",
            slot = slot.index(),
            codec = ?codec,
            max_width = request.limits.max_width,
            max_height = request.limits.max_height,
            max_fps = request.limits.max_fps,
            width = video.width,
            height = video.height,
            quality = ?request.quality,
        );
        let mut release = SlotRelease {
            shared: &self.shared,
            room: room.clone(),
            slot,
            conn_id,
            armed: true,
        };
        let session_cancel = session.shared.cancel.clone();
        let worker_room = room.clone();
        session::run(session.clone(), stream, async move {
            worker_room.wait_for_session(session_cancel).await
        })
        .await;
        // Recorded before the slot frees, so a viewer that reconnects once
        // this one is gone starts from it. A session that leaves none keeps
        // the record as it was, with its own end.
        if let Some(rate) = session.closing_rate(Instant::now()) {
            *self.shared.closing_rate.lock() = Some(rate);
        }
        {
            let mut active = self.room.lock().await;
            release.armed = false;
            let empty = room.detach(slot, conn_id);
            if empty || room.is_closed() {
                if active.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &room)) {
                    active.take();
                }
                self.shared.clear_room(&room);
            }
            // The status lists this viewer no longer: said before an emptied
            // room's thread C is waited for.
            self.shared.changes.bump();
            if empty {
                room.shutdown().await;
                self.shared.restore_pending.store(true, Ordering::Release);
                self.shared.restore.notify_one();
            }
        }
        // The slot frees before the permit, which a panic keeps too: `permit`
        // was bound first, so it is dropped last.
        drop(release);
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
        let mut active = self.room.lock().await;
        if let Some(existing) = active.as_ref().cloned()
            && existing.is_closed()
        {
            active.take();
            self.shared.clear_room(&existing);
            self.shared.changes.bump();
            existing.shutdown().await;
            self.shared.restore_pending.store(true, Ordering::Release);
            self.shared.restore.notify_one();
        }
        let offered = request
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_H265);
        if let Some(room) = active.as_ref().cloned() {
            let Some((codec, incompatible)) = room.negotiate_codec(&request.codecs, offered) else {
                drop(active);
                send_rejected(stream, RejectReason::Busy).await;
                return None;
            };
            let Some(slot) = room.free_slot() else {
                drop(active);
                send_rejected(stream, RejectReason::Busy).await;
                return None;
            };
            let video = expected_video(room.display.as_ref(), request.limits);
            let capabilities =
                negotiated_capabilities(&room.host_capabilities, &request.capabilities);
            let accepted = HostMessage::Accepted {
                codec,
                limits: request.limits,
                nominal_fps: request.limits.max_fps,
                quality: request.quality,
                capabilities: capabilities.clone(),
            };
            tokio::time::timeout(
                HANDSHAKE_TIMEOUT,
                wire::write_message(&mut stream, &accepted, None),
            )
            .await
            .ok()?
            .ok()?;
            if !incompatible.is_empty() || codec != room.current_codec() {
                let message = stop_message(StopReason::Restarting);
                for session in incompatible {
                    session.end(StopReason::Restarting, message);
                }
                room.set_codec(codec);
            }
            // A closed session's rate starts only a session that opens a
            // room: one that joins shares the encoder, which runs at the
            // least of its viewers' rates, with viewers it would pull down.
            let session = Session::new_in_room(
                slot,
                room.shared.wake.clone(),
                room.shared.peer_cursors.clone(),
                self.shared.policy.clone(),
                self.cancel.child_token(),
                codec,
                request.codecs.clone(),
                request.limits,
                request.quality,
                room.rate_model,
                None,
                &request.capabilities,
                &capabilities,
                self.shared.changes.clone(),
            );
            session.expect_video_size(video);
            let conn_id = room.attach(slot, session.clone(), request.limits)?;
            self.shared.changes.bump();
            return Some(Admitted {
                stream,
                permit,
                request,
                codec,
                video,
                room,
                slot,
                conn_id,
                session,
            });
        }
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
        let display = description
            .as_ref()
            .map(|described| described.display.clone());
        let video = expected_video(display.as_ref(), request.limits);
        let caps =
            description.map_or_else(|| self.launch.fallback_caps(), |described| described.caps);
        let h265 = match prepared.h265.as_mut() {
            Some(answer) => matches!(
                tokio::time::timeout_at(deadline, answer).await,
                Ok(Ok(true))
            ),
            None => false,
        };
        let can_h265 = probe && h265;
        let codec = choose_codec(&request.codecs, can_h265);
        let host_capabilities = caps.values();
        let capabilities = negotiated_capabilities(&host_capabilities, &request.capabilities);
        let accepted = HostMessage::Accepted {
            codec,
            limits: request.limits,
            nominal_fps: request.limits.max_fps,
            quality: request.quality,
            capabilities: capabilities.clone(),
        };
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            wire::write_message(&mut stream, &accepted, None),
        )
        .await
        .ok()?
        .ok()?;
        let room_shared = RoomShared::new(self.shared.policy.clone(), codec);
        let slot = PeerSlot::FIRST;
        let previous = *self.shared.closing_rate.lock();
        let session = Session::new_in_room(
            slot,
            room_shared.wake.clone(),
            room_shared.peer_cursors.clone(),
            self.shared.policy.clone(),
            self.cancel.child_token(),
            codec,
            request.codecs.clone(),
            request.limits,
            request.quality,
            caps.rate_model,
            previous,
            &request.capabilities,
            &capabilities,
            self.shared.changes.clone(),
        );
        session.expect_video_size(video);
        room_shared.set_slot(
            slot,
            1,
            RoomSlotConfig {
                shared: session.shared.clone(),
                color: PeerColor::default(),
                limits: request.limits,
                cursor_shapes: session.cursor_shape,
                peer_cursors: session.peer_cursors,
            },
        );
        let worker = prepared.start_room(room_shared.clone(), self.admission.clone());
        let room = Room::new_with_first_slot(
            room_shared,
            worker,
            host_capabilities,
            display,
            can_h265,
            caps.rate_model,
            codec,
            session.clone(),
        );
        *active = Some(room.clone());
        *self.shared.room.lock() = Some(room.clone());
        self.shared.changes.bump();
        Some(Admitted {
            stream,
            permit,
            request,
            codec,
            video,
            room,
            slot,
            conn_id: 1,
            session,
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
        send_rejected(stream, rejected).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendCaps, HostOs, ViewerConfig, protocol::MAX_CAPABILITIES};

    /// The most a host lists, for a viewer that lists the most it can: a
    /// backend that can do everything, and every capability every host
    /// speaks. Both lists stay within `MAX_CAPABILITIES`, so neither end ever
    /// refuses the other's as malformed.
    #[test]
    fn the_longest_lists_stay_within_the_bound() {
        let mut config = ViewerConfig::new(vec![VideoCodec::H265, VideoCodec::H264]);
        config.fps_override = true;
        let viewer = config.capabilities();
        assert!(valid_capabilities(&viewer), "{viewer:?}");
        let backend = BackendCaps {
            h265: true,
            lock: true,
            display_modes: true,
            os: Some(HostOs::Android),
            physical_keys: true,
            cursor_shapes: true,
            ..BackendCaps::default()
        };
        let host = negotiated_capabilities(&backend.values(), &viewer);
        for wanted in SESSION_CAPABILITIES {
            assert!(host.iter().any(|value| value == wanted), "{host:?}");
        }
        assert!(valid_capabilities(&host), "{host:?}");
        assert!(viewer.len().max(host.len()) <= MAX_CAPABILITIES);
        // A viewer that lists none of them hears of none.
        let host = negotiated_capabilities(&backend.values(), &[]);
        assert!(
            SESSION_CAPABILITIES
                .iter()
                .all(|wanted| !host.iter().any(|value| value == wanted)),
            "{host:?}"
        );
    }
}

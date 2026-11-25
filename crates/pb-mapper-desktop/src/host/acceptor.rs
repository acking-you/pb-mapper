//! The host's listener: each connection's handshake and session.

use super::{BACKEND_ANSWER_WAIT, Room, Shared, TicketDigest, ticket_digest};
use crate::{
    Display, QualityPreset, Result, VideoCodec, VideoLimits, VideoSize,
    launch::Launch,
    protocol::{
        CAPABILITY_ARRIVAL_TIME, CAPABILITY_H265, CAPABILITY_KEYFRAME_SEQ, CAPABILITY_PEER_CURSORS,
        CAPABILITY_STANDBY, HostMessage, PeerColor, PeerSlot, RejectReason, StopReason,
        ViewerMessage, valid_capabilities,
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
    /// The ticket of the pair this connection asks to stand by for.
    standby_for: Option<String>,
}
impl Request {
    fn listed(&self, wanted: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == wanted)
    }
}

/// A viewer that holds a room slot, or stands by for one, and heard
/// `Accepted`. The room holds its admission permit.
struct Admitted {
    stream: TlsStream<TcpStream>,
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
const SESSION_CAPABILITIES: [&str; 4] = [
    CAPABILITY_PEER_CURSORS,
    CAPABILITY_ARRIVAL_TIME,
    CAPABILITY_KEYFRAME_SEQ,
    CAPABILITY_STANDBY,
];

/// A new pair's ticket, as `Accepted` carries it and as the room keeps it:
/// 32 bytes from the system's generator. `None` when it has none to give,
/// which leaves the pair without standbys.
fn new_ticket() -> Option<(String, TicketDigest)> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0; 32];
    SystemRandom::new().fill(&mut bytes).ok()?;
    let ticket = URL_SAFE_NO_PAD.encode(bytes);
    let digest = ticket_digest(&ticket);
    Some((ticket, digest))
}

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
    /// Up to `MAX_VIEWERS` standbys, one per slot at most.
    pub(super) standby: Arc<Semaphore>,
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
            standby = request.standby_for.is_some(),
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
        // the record as it was, with its own end, and so does a standby,
        // which never sent video.
        if !session.standing_by()
            && let Some(rate) = session.closing_rate(Instant::now())
        {
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
        // The room holds the permit, which frees with the slot.
        drop(release);
    }

    /// The handshake, then `Accepted` with the session's codec and
    /// capabilities. Thread C starts building the backend as soon as the
    /// viewer holds the slot, and `Accepted` waits until the backend described
    /// itself: which display it captures, which sizes the first IDR's budget;
    /// what it can do; and, for a viewer that decodes H.265, whether it
    /// encodes H.265, unless an H.265 encoder already failed in this process.
    async fn accept_session(&self, socket: TcpStream) -> Option<Admitted> {
        let (mut stream, permit, request) = self.handshake(socket).await?;
        if let Some(ticket) = request.standby_for.as_deref() {
            let ticket = ticket_digest(ticket);
            return self.accept_standby(stream, permit, request, ticket).await;
        }
        // A pair's ticket, for a viewer that may open standbys.
        let (ticket, digest) = request
            .listed(CAPABILITY_STANDBY)
            .then(new_ticket)
            .flatten()
            .unzip();
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
                ticket: ticket.clone(),
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
            // room: one that joins shares an encoder that runs at the least
            // of its viewers' rates, and would pull down those watching.
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
            let conn_id = room.attach(slot, session.clone(), request.limits, permit, digest)?;
            self.shared.changes.bump();
            return Some(Admitted {
                stream,
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
            ticket,
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
                succeeds: None,
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
            (session.clone(), permit, digest),
        );
        *active = Some(room.clone());
        *self.shared.room.lock() = Some(room.clone());
        self.shared.changes.bump();
        Some(Admitted {
            stream,
            request,
            codec,
            video,
            room,
            slot,
            conn_id: 1,
            session,
        })
    }

    /// A standby's admission, its token checked first: then its ticket, in
    /// constant time over the digests, against every pair's. It joins the
    /// pair whose ticket it presented, on that pair's codec and paused:
    /// thread C knows nothing of it, so it causes no keyframe, shows in no
    /// status and has no pointer, until it takes the pair's place. A
    /// ticket no pair holds is denied; a pair that has a standby is busy.
    async fn accept_standby(
        &self,
        mut stream: TlsStream<TcpStream>,
        permit: OwnedSemaphorePermit,
        request: Request,
        ticket: TicketDigest,
    ) -> Option<Admitted> {
        let room = self.room.lock().await.clone();
        let Some((room, slot, codec)) = room.and_then(|room| {
            let (slot, codec) = room.pair(&ticket)?;
            Some((room, slot, codec))
        }) else {
            send_rejected(stream, RejectReason::Denied).await;
            return None;
        };
        // The pair's codec, which its viewer offered when it opened it.
        let offered = match codec {
            VideoCodec::H265 => request.listed(CAPABILITY_H265),
            codec => request.codecs.contains(&codec),
        };
        if !offered {
            send_rejected(stream, RejectReason::InvalidRequest).await;
            return None;
        }
        let video = expected_video(room.display.as_ref(), request.limits);
        let capabilities = negotiated_capabilities(&room.host_capabilities, &request.capabilities);
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
        let conn_id =
            match room.attach_standby(slot, &ticket, session.clone(), request.limits, permit) {
                Ok(conn_id) => conn_id,
                Err(rejected) => {
                    send_rejected(stream, rejected).await;
                    return None;
                }
            };
        let accepted = HostMessage::Accepted {
            codec,
            limits: request.limits,
            nominal_fps: request.limits.max_fps,
            quality: request.quality,
            capabilities,
            ticket: None,
        };
        let written = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            wire::write_message(&mut stream, &accepted, None),
        )
        .await;
        if !matches!(written, Ok(Ok(()))) {
            session.close();
            room.detach(slot, conn_id);
            return None;
        }
        Some(Admitted {
            stream,
            request,
            codec,
            video,
            room,
            slot,
            conn_id,
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
            standby_for,
        })) = wire::read_header(&mut stream, remaining).await
        else {
            return None;
        };
        // A standby takes a standby's permit, never a viewer slot's.
        let permits = if standby_for.is_some() {
            &self.standby
        } else {
            &self.slot
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
        } else if let Ok(permit) = permits.clone().try_acquire_owned() {
            return Some((
                stream,
                permit,
                Request {
                    codecs,
                    limits,
                    quality,
                    capabilities,
                    standby_for,
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
            unlock: true,
            display_modes: true,
            os: Some(HostOs::Android),
            physical_keys: true,
            cursor_shapes: true,
            hires_wheel: true,
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

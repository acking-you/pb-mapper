use crate::{
    Frame, InputEvent, Invitation, Result, SessionStatus, error, security,
    wire::{self, Command},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{Client, ConnectOptions, Connection};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

pub struct Viewer {
    presented: watch::Sender<(u64, u64)>,
    input: mpsc::Sender<Command>,
    priority: Arc<Controls>,
    frames: watch::Receiver<Option<Frame>>,
    media: Arc<crate::media::Queue>,
    keyframe: watch::Sender<u64>,
    status: Arc<Mutex<(SessionStatus, Instant)>>,
    cancel: CancellationToken,
    connection: Arc<Connection>,
    join: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
}

// Releases are state, not a queue of gestures. Repeated focus/pointer-cancel
// notifications must not disconnect video. A revoke always reaches the peer
// before a subsequent grant, and every barrier discards obsolete input.
#[derive(Default)]
struct Controls {
    pending: Mutex<PendingControls>,
    changed: Notify,
}
#[derive(Default)]
struct PendingControls {
    revoke: bool,
    release: bool,
    grant: bool,
}
impl Controls {
    fn push(&self, command: Command) {
        let mut state = self.pending.lock();
        match command {
            Command::Control { enabled: false } => {
                state.revoke = true;
                state.grant = false;
                state.release = false;
            }
            Command::Control { enabled: true } => state.grant = true,
            Command::Release => state.release = true,
            _ => unreachable!("only control barriers enter this mailbox"),
        }
        drop(state);
        self.changed.notify_one();
    }
    fn take(&self) -> Option<Command> {
        let mut state = self.pending.lock();
        if std::mem::take(&mut state.revoke) {
            Some(Command::Control { enabled: false })
        } else if std::mem::take(&mut state.release) {
            Some(Command::Release)
        } else if std::mem::take(&mut state.grant) {
            Some(Command::Control { enabled: true })
        } else {
            None
        }
    }
    async fn next(&self) -> Command {
        loop {
            let changed = self.changed.notified();
            if let Some(command) = self.take() {
                return command;
            }
            changed.await;
        }
    }
}
impl Viewer {
    pub async fn connect(
        client: &Client,
        invitation: Invitation,
        options: ConnectOptions,
    ) -> Result<Self> {
        Self::connect_with_codecs(client, invitation, options, vec![crate::VideoCodec::Jpeg]).await
    }
    /// Select codecs supported by the embedding renderer. H264 frames must be
    /// consumed in order using `next_frame`; latest-frame polling loses refs.
    pub async fn connect_with_codecs(
        client: &Client,
        invitation: Invitation,
        options: ConnectOptions,
        codecs: Vec<crate::VideoCodec>,
    ) -> Result<Self> {
        Self::connect_with_video_preferences(
            client,
            invitation,
            options,
            codecs,
            crate::VideoPreferences::default(),
        )
        .await
    }
    /// Connect with the renderer's bounded video limits. H264 access units must
    /// still be consumed in order through `next_frame`.
    pub async fn connect_with_video_preferences(
        client: &Client,
        invitation: Invitation,
        options: ConnectOptions,
        codecs: Vec<crate::VideoCodec>,
        preferences: crate::VideoPreferences,
    ) -> Result<Self> {
        Self::connect_inner(client, invitation, options, codecs, preferences, false).await
    }
    /// Opt into bounded media admission and adaptation driven by actual renderer
    /// presentation. Call `presented` only after displaying a frame; transport
    /// benchmarks and receivers without a renderer must use the existing APIs.
    pub async fn connect_with_presentation_feedback(
        client: &Client,
        invitation: Invitation,
        options: ConnectOptions,
        codecs: Vec<crate::VideoCodec>,
        preferences: crate::VideoPreferences,
    ) -> Result<Self> {
        Self::connect_inner(client, invitation, options, codecs, preferences, true).await
    }
    async fn connect_inner(
        client: &Client,
        invitation: Invitation,
        options: ConnectOptions,
        codecs: Vec<crate::VideoCodec>,
        preferences: crate::VideoPreferences,
        presentation_feedback: bool,
    ) -> Result<Self> {
        preferences.validate()?;
        if codecs.is_empty() || codecs.len() > 2 {
            return Err(error("unsupported codec selection"));
        }
        // Validate SDK callers too, not just the UI parser.
        let invitation = Invitation::decode(&invitation.encode()?)?;
        let tls = security::client(&invitation.certificate)?;
        let (stream, connection) = client
            .connect_tcp(invitation.service, options)
            .await
            .map_err(error)?;
        let mut stream = tokio::time::timeout(
            Duration::from_secs(15),
            tls.connect(
                quinn::rustls::pki_types::ServerName::try_from(security::NAME).map_err(error)?,
                stream,
            ),
        )
        .await
        .map_err(error)??;
        tokio::time::timeout(
            wire::IO_TIMEOUT,
            wire::write_json(
                &mut stream,
                &Command::Authenticate {
                    token: invitation.token,
                    codecs,
                    video: Some(preferences),
                    presentation_feedback,
                },
            ),
        )
        .await
        .map_err(error)??;
        let connection = Arc::new(connection);
        let status = Arc::new(Mutex::new((
            SessionStatus {
                state: "connecting".into(),
                ..Default::default()
            },
            Instant::now(),
        )));
        let (frames_tx, frames) = watch::channel(None);
        let media = Arc::new(crate::media::Queue::default());
        let incoming = media.clone();
        let (keyframe, mut keyframes) = watch::channel(0u64);
        let recover = keyframe.clone();
        let (input, mut inputs) = mpsc::channel(32);
        let priority = Arc::new(Controls::default());
        let controls = priority.clone();
        let (presented, mut presentation) = watch::channel((0, 0));
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let observation = status.clone();
        let tunnel = connection.clone();
        let join = tokio::spawn(async move {
            let (mut read, mut write) = tokio::io::split(stream);
            let receive = async {
                let mut sample_at = Instant::now();
                let mut sample_frames = 0u32;
                let mut receive_fps = 0f32;
                loop {
                    let (mut state, jpeg) =
                        tokio::time::timeout(wire::IO_TIMEOUT + Duration::from_secs(1), async {
                            let state = wire::read_json::<_, SessionStatus>(&mut read).await?;
                            let bytes = wire::read_bytes(&mut read, wire::MAX_FRAME).await?;
                            Ok::<_, std::io::Error>((state, bytes))
                        })
                        .await
                        .map_err(error)??;
                    state.path = tunnel.diagnostics().direct.map_or_else(
                        || "relay".into(),
                        |d| {
                            if d.active_direct > 0 {
                                "direct".into()
                            } else {
                                "relay".into()
                            }
                        },
                    );
                    if state.display.as_ref().is_some_and(|d| {
                        d.width == 0 || d.height == 0 || d.width > 32768 || d.height > 32768
                    }) {
                        return Err(error("invalid display geometry"));
                    }
                    if state.video.codec == crate::VideoCodec::H264
                        && (state.video.width == 0
                            || state.video.height == 0
                            || state.video.width > preferences.max_width
                            || state.video.height > preferences.max_height)
                    {
                        return Err(error("invalid video geometry"));
                    }
                    if !jpeg.is_empty() && !state.video.duplicate {
                        sample_frames += 1;
                    }
                    if sample_at.elapsed() >= Duration::from_secs(1) {
                        receive_fps = sample_frames as f32 / sample_at.elapsed().as_secs_f32();
                        sample_at = Instant::now();
                        sample_frames = 0;
                    }
                    state.receive_fps = receive_fps;
                    state.dropped_frames += incoming.dropped();
                    if state.state == "stopped" {
                        return Err(error(if state.message.is_empty() {
                            "Desktop host stopped the session"
                        } else {
                            &state.message
                        }));
                    }
                    if jpeg.is_empty() && state.state == "streaming" {
                        // An admission-window heartbeat is not a fresh picture.
                        // Preserve image age and decoder references while still
                        // servicing transport liveness and status observation.
                        observation.lock().0 = state;
                        continue;
                    }
                    *observation.lock() = (state.clone(), Instant::now());
                    let frame = Frame {
                        status: state,
                        bytes: jpeg.into(),
                    };
                    incoming.push(frame.clone());
                    if incoming.take_keyframe_request() {
                        recover.send_modify(|v| *v = v.wrapping_add(1));
                    }
                    frames_tx.send_replace(Some(frame));
                }
            };
            let send = async {
                let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
                loop {
                    let command = tokio::select! {
                        biased;
                        command = controls.next() => { while inputs.try_recv().is_ok() {} command },
                        Ok(()) = keyframes.changed() => { keyframes.borrow_and_update(); Command::Keyframe },
                        Ok(()) = presentation.changed() => { let (generation, frame_id) = *presentation.borrow_and_update(); Command::Presented { generation, frame_id } },
                        _ = heartbeat.tick() => Command::Ping,
                        Some(command) = inputs.recv() => command,
                        else => return Ok::<_, std::io::Error>(()),
                    };
                    tokio::time::timeout(wire::IO_TIMEOUT, wire::write_json(&mut write, &command))
                        .await
                        .map_err(error)??;
                }
            };
            let result = tokio::select! { () = stop.cancelled() => Ok(()), result = receive => result, result = send => result };
            stop.cancel();
            let mut current = observation.lock();
            current.0.state = "stopped".into();
            current.0.controlling = false;
            if let Err(e) = result {
                current.0.message = e.to_string();
            }
        });
        Ok(Self {
            presented,
            input,
            priority,
            frames,
            media,
            keyframe,
            status,
            cancel,
            connection,
            join: AsyncMutex::new(Some(join)),
        })
    }
    pub fn status(&self) -> SessionStatus {
        let current = self.status.lock();
        let mut state = current.0.clone();
        if state.state == "streaming" && current.1.elapsed() > Duration::from_secs(2) {
            state.state = "stale".into();
            state.controlling = false;
        }
        state
    }
    /// Call only after a frame was actually presented. Merely receiving video
    /// does not extend held input when a renderer is frozen or in background.
    pub fn presented(&self, generation: u64, frame_id: u64) {
        self.presented.send_replace((generation, frame_id));
    }
    pub fn frames(&self) -> watch::Receiver<Option<Frame>> {
        self.frames.clone()
    }
    pub fn latest_frame(&self) -> Option<Frame> {
        self.frames.borrow().clone()
    }
    pub fn next_frame(&self) -> Option<Frame> {
        self.media.pop()
    }
    /// Renderer overflow/reset requires a new independently decodable image.
    pub fn request_keyframe(&self) {
        self.keyframe.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub fn request_control(&self, enabled: bool) -> Result<()> {
        self.control_command(Command::Control { enabled })
    }
    pub fn release_input(&self) -> Result<()> {
        self.control_command(Command::Release)
    }
    fn control_command(&self, command: Command) -> Result<()> {
        if self.cancel.is_cancelled() {
            // Teardown already releases remote input. Idempotent release/end
            // calls must not replace the original transport failure message.
            return match command {
                Command::Release | Command::Control { enabled: false } => Ok(()),
                _ => Err(error("desktop session is disconnected")),
            };
        }
        self.priority.push(command);
        Ok(())
    }
    pub fn input(&self, event: InputEvent) -> Result<()> {
        let status = self.status();
        if status.state != "streaming"
            || !status.controlling
            || status.generation != event.generation
        {
            return Err(error("a current screen and control grant are required"));
        }
        if self.input.try_send(Command::Input { event }).is_err() {
            // Preserve video while releasing every held input. The priority
            // barrier drains queued gestures; no key-up is silently lost and
            // no old click is retried when the transport recovers.
            self.priority.push(Command::Control { enabled: false });
            self.status.lock().0.controlling = false;
            return Err(error(
                "input congestion; control released while video recovers",
            ));
        }
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        self.connection.stop().await.map_err(error)
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repeated_releases_are_bounded_and_revocation_precedes_a_new_grant() {
        let controls = Controls::default();
        for _ in 0..1000 {
            controls.push(Command::Release);
        }
        assert!(matches!(controls.next().await, Command::Release));
        assert!(controls.take().is_none());
        controls.push(Command::Control { enabled: true });
        controls.push(Command::Control { enabled: false });
        controls.push(Command::Control { enabled: true });
        assert!(matches!(
            controls.next().await,
            Command::Control { enabled: false }
        ));
        assert!(matches!(
            controls.next().await,
            Command::Control { enabled: true }
        ));
        assert!(controls.take().is_none());
    }
}

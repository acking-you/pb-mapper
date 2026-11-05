use crate::{
    Invitation, Result, SessionStatus, error, platform,
    wire::{self, Command},
    worker::{Factory, Policy, Worker},
};
use parking_lot::Mutex;
use pb_mapper_client::sdk::{Client, RegisterOptions, RegisterRequest, Registration, Transport};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::Mutex as AsyncMutex, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub struct HostOptions {
    pub service: String,
    pub display_id: u32,
    pub allow_control: bool,
    pub allow_direct: bool,
}

pub struct Host {
    invitation: Invitation,
    allowed: Arc<Policy>,
    status: Arc<Mutex<SessionStatus>>,
    cancel: CancellationToken,
    registration: Registration,
    join: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Host {
    pub async fn start(client: &Client, options: HostOptions) -> Result<Self> {
        let id = options.display_id;
        Self::start_with_backend(
            client,
            options,
            Arc::new(move || platform::native_backend(id)),
        )
        .await
    }
    pub async fn start_with_backend(
        client: &Client,
        options: HostOptions,
        factory: Factory,
    ) -> Result<Self> {
        Self::start_with_admission(client, options, factory, None).await
    }
    pub async fn start_with_admission(
        client: &Client,
        options: HostOptions,
        factory: Factory,
        admission: Option<Arc<dyn crate::ControlAdmission>>,
    ) -> Result<Self> {
        Self::start_with_backend_and_identity(
            client,
            options,
            factory,
            admission,
            crate::HostIdentity::generate()?,
        )
        .await
    }
    /// Resume a previously and explicitly authorized grant. Dropping or stopping
    /// this host closes sessions; revoke durable access by discarding the saved
    /// identity and generating a new one before the next start.
    pub async fn start_with_identity(
        client: &Client,
        options: HostOptions,
        identity: crate::HostIdentity,
    ) -> Result<Self> {
        let id = options.display_id;
        Self::start_with_backend_and_identity(
            client,
            options,
            Arc::new(move || platform::native_backend(id)),
            None,
            identity,
        )
        .await
    }
    pub async fn start_with_backend_and_identity(
        client: &Client,
        options: HostOptions,
        factory: Factory,
        admission: Option<Arc<dyn crate::ControlAdmission>>,
        identity: crate::HostIdentity,
    ) -> Result<Self> {
        if !crate::valid_service(&options.service) {
            return Err(error(
                "desktop service must start with desktop: and contain at most 128 ASCII letters, numbers, periods, hyphens, underscores or colons",
            ));
        }
        let tls = identity.acceptor()?;
        let invitation = identity.invitation(options.service.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key: options.service,
                    local_addr: listener.local_addr()?.to_string(),
                    transport: Transport::Tcp,
                    codec: true,
                    force_namespace: false,
                },
                RegisterOptions {
                    allow_direct: options.allow_direct,
                    ..Default::default()
                },
            )
            .await
            .map_err(error)?;
        registration
            .wait_ready_timeout(Duration::from_secs(15))
            .await
            .map_err(error)?;
        let allowed = Arc::new(Policy::new(options.allow_control));
        let status = Arc::new(Mutex::new(SessionStatus {
            state: "waiting".into(),
            control_allowed: options.allow_control,
            ..Default::default()
        }));
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let permission = allowed.clone();
        let observation = status.clone();
        let token = invitation.token.clone();
        let join = tokio::spawn(async move {
            let active = Arc::new(tokio::sync::Semaphore::new(1));
            let mut sessions = JoinSet::new();
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    Some(_) = sessions.join_next(), if !sessions.is_empty() => {},
                    result = listener.accept(), if sessions.len() < 8 => {
                        let Ok((socket, _)) = result else { break };
                        let (tls, token, active, factory, allowed, status, stop) = (tls.clone(), token.clone(), active.clone(), factory.clone(), permission.clone(), observation.clone(), stop.clone());
                        let admission = admission.clone();
                        sessions.spawn(async move {
                            let handshake = async {
                                // Status and small video access units must not
                                // wait for Nagle/delayed-ACK coalescing on the
                                // publisher's accepted loopback connection.
                                socket.set_nodelay(true)?;
                                let mut stream = tls.accept(socket).await?;
                                let Command::Authenticate { token: presented, codecs, video, presentation_feedback } = wire::read_json(&mut stream).await? else { return Err(error("authentication required")) };
                                if codecs.len() > 2 { return Err(error("too many video codecs")); }
                                let codec = if codecs.contains(&crate::VideoCodec::H264) { crate::VideoCodec::H264 } else { crate::VideoCodec::Jpeg };
                                use ring::digest::{digest, SHA256};
                                use subtle::ConstantTimeEq;
                                if !bool::from(digest(&SHA256, presented.as_bytes()).as_ref().ct_eq(digest(&SHA256, token.as_bytes()).as_ref())) { return Err(error("desktop permission denied")); }
                                let preferences = video.unwrap_or_default();
                                preferences.validate()?;
                                let permit = active.try_acquire_owned().map_err(|_| error("a viewer is already connected"))?;
                                Ok((stream, permit, codec, preferences, presentation_feedback))
                            };
                            let connected = tokio::select! {
                                () = stop.cancelled() => return,
                                result = tokio::time::timeout(wire::IO_TIMEOUT, handshake) => result,
                            };
                            let Ok(Ok((stream, _permit, codec, preferences, presentation_feedback))) = connected else { return };
                            let session_status = status.clone();
                            let mut worker = Worker::start_video(factory, allowed, status, codec, preferences, presentation_feedback, admission);
                            let (mut read, mut write) = tokio::io::split(stream);
                            let mut frames = worker.frames.clone();
                            let receive = async {
                                loop {
                                    let command = tokio::time::timeout(wire::IO_TIMEOUT, wire::read_json::<_, Command>(&mut read)).await.map_err(error)??;
                                    // Permission/stale input errors are reported with the next
                                    // status frame; malformed frames terminate the transport.
                                    let _ = worker.command(command).await;
                                }
                                #[allow(unreachable_code)] Ok::<(), std::io::Error>(())
                            };
                            let send = async {
                                let mut pending = None;
                                let mut last_sent = std::time::Instant::now();
                                loop {
                                    if worker.feedback.as_ref().is_some_and(|feedback| feedback.stalled()) {
                                        let mut state = session_status.lock().clone();
                                        state.state = "stopped".into();
                                        state.controlling = false;
                                        state.message = "Renderer stopped acknowledging presented frames; reconnect the viewer".into();
                                        tokio::time::timeout(wire::IO_TIMEOUT, async {
                                            wire::write_json(&mut write, &state).await?;
                                            wire::write_bytes(&mut write, &[], wire::MAX_FRAME).await
                                        }).await.map_err(error)??;
                                        return Err(error(&state.message));
                                    }
                                    if pending.is_none() { pending = worker.media.pop(); }
                                    let mut waiting = false;
                                    if let Some(frame) = pending.as_ref()
                                        && let Some(feedback) = &worker.feedback {
                                            let state = serde_json::to_vec(&frame.status).map_err(error)?;
                                            match feedback.admit(frame, state.len() + frame.bytes.len() + 8) {
                                                crate::congestion::Admission::Send => {},
                                                crate::congestion::Admission::Wait => waiting = true,
                                                crate::congestion::Admission::Discard => { pending = None; worker.media.resynchronize(); }
                                            }
                                    }
                                    if pending.is_none() || waiting {
                                        if let Some(feedback) = &worker.feedback {
                                            if last_sent.elapsed() >= Duration::from_secs(1) {
                                                // Transport liveness without inventing a new
                                                // image/presentation or renewing input freshness.
                                                let state = session_status.lock().clone();
                                                tokio::time::timeout(wire::IO_TIMEOUT, async {
                                                    wire::write_json(&mut write, &state).await?;
                                                    wire::write_bytes(&mut write, &[], wire::MAX_FRAME).await
                                                }).await.map_err(error)??;
                                                last_sent = std::time::Instant::now();
                                            }
                                            tokio::select! { result = frames.changed() => { result.map_err(error)?; }, () = feedback.changed.notified() => {}, () = tokio::time::sleep(Duration::from_millis(20)) => {} }
                                        } else { frames.changed().await.map_err(error)?; }
                                        continue;
                                    }
                                    let frame = pending.take().ok_or_else(|| error("missing admitted desktop frame"))?;
                                    let started = std::time::Instant::now();
                                    tokio::time::timeout(wire::IO_TIMEOUT, async {
                                        wire::write_json(&mut write, &frame.status).await?;
                                        wire::write_bytes(&mut write, &frame.bytes, wire::MAX_FRAME).await
                                    }).await.map_err(error)??;
                                    worker.sent(frame.bytes.len(), started.elapsed());
                                    last_sent = std::time::Instant::now();
                                }
                                #[allow(unreachable_code)] Ok::<(), std::io::Error>(())
                            };
                            let outcome = tokio::select! { () = stop.cancelled() => Ok(()), result = receive => result, result = send => result };
                            worker.stop().await;
                            if let Err(e) = outcome { session_status.lock().message = e.to_string(); }
                        });
                    }
                }
            }
            while sessions.join_next().await.is_some() {}
        });
        Ok(Self {
            invitation,
            allowed,
            status,
            cancel,
            registration,
            join: AsyncMutex::new(Some(join)),
        })
    }
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    pub fn status(&self) -> SessionStatus {
        let mut status = self.status.lock().clone();
        status.control_allowed = self.allowed.allowed();
        if !status.control_allowed {
            status.controlling = false;
        }
        status
    }
    /// Revocation gates every subsequent input before it reaches the backend.
    /// The worker releases held state at its next scheduling point.
    pub fn set_control_allowed(&self, allowed: bool) {
        self.allowed.set(allowed);
        self.status.lock().control_allowed = allowed;
    }
    pub async fn stop(&self) -> Result<()> {
        self.allowed.set(false);
        self.cancel.cancel();
        let mut join = self.join.lock().await;
        if let Some(task) = join.as_mut() {
            let _ = task.await;
        }
        join.take();
        self.registration.stop().await.map_err(error)?;
        self.status.lock().state = "stopped".into();
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.allowed.set(false);
        self.cancel.cancel();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod trust_tests {
    use super::*;
    struct Screen;
    impl crate::Backend for Screen {
        fn display(&mut self) -> Result<crate::Display> {
            Ok(crate::Display {
                id: 1,
                name: "trusted".into(),
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            })
        }
        fn capture(&mut self) -> Result<Vec<u8>> {
            Ok(vec![1])
        }
        fn input(&mut self, _: &crate::Input) -> Result<()> {
            Err(error("read-only test backend"))
        }
        fn release(&mut self) {}
    }
    fn options() -> HostOptions {
        HostOptions {
            service: "desktop:remembered".into(),
            display_id: 1,
            allow_control: false,
            allow_direct: false,
        }
    }
    #[tokio::test]
    async fn remembered_invitation_survives_restart_but_not_revocation() {
        let relay = pb_mapper_testkit::Relay::start("desktop-trust").await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let identity = crate::HostIdentity::generate().unwrap();
        let saved = identity.encode_secret().unwrap();
        let factory: Factory = Arc::new(|| Ok(Box::new(Screen)));
        let host = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory.clone(),
            None,
            identity,
        )
        .await
        .unwrap();
        let invitation = host.invitation().clone();
        host.stop().await.unwrap();
        let resumed = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory.clone(),
            None,
            crate::HostIdentity::decode_secret(&saved).unwrap(),
        )
        .await
        .unwrap();
        let viewer = crate::Viewer::connect(
            &client,
            invitation.clone(),
            pb_mapper_client::sdk::ConnectOptions {
                path: pb_mapper_client::sdk::PathPolicy::RelayOnly,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut frames = viewer.frames();
        tokio::time::timeout(Duration::from_secs(5), frames.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!frames.borrow().as_ref().unwrap().status.control_allowed);
        viewer.stop().await.unwrap();
        resumed.stop().await.unwrap();
        let replacement = Host::start_with_backend_and_identity(
            &client,
            options(),
            factory,
            None,
            crate::HostIdentity::generate().unwrap(),
        )
        .await
        .unwrap();
        assert!(
            crate::Viewer::connect(
                &client,
                invitation,
                pb_mapper_client::sdk::ConnectOptions {
                    path: pb_mapper_client::sdk::PathPolicy::RelayOnly,
                    ..Default::default()
                }
            )
            .await
            .is_err()
        );
        replacement.stop().await.unwrap();
    }
}

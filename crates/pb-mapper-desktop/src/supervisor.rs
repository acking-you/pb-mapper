//! Reconnecting a viewer by policy: backoff within a window, a limit on
//! protocol failures in a row, a stall that counts as an end, and H.265
//! falling back to H.264 after a decoder failure.
//!
//! [`Reconnect`] is the policy alone, a pure state machine an app with its
//! own timers can drive; [`Supervisor`] drives it, connecting and waiting.
//!
//! ```no_run
//! # async fn example(
//! #     client: pb_mapper_client::sdk::Client,
//! #     invitation: pb_mapper_desktop::Invitation,
//! # ) {
//! use pb_mapper_client::sdk::ConnectOptions;
//! use pb_mapper_desktop::{VideoCodec, ViewerConfig, supervisor::Supervisor};
//!
//! let config = ViewerConfig::new(vec![VideoCodec::H265, VideoCodec::H264]);
//! let mut supervisor = Supervisor::new(client, invitation, ConnectOptions::default(), config);
//! while let Some(attempt) = supervisor.next().await {
//!     match attempt {
//!         // Attach a consumer, present, send input, as with any viewer.
//!         Ok(viewer) => println!("ended: {:?}", supervisor.ended(&viewer).await.reason),
//!         Err(failure) => println!("could not connect: {failure}"),
//!     }
//! }
//! # }
//! ```
use crate::{
    ConnectError, DecoderState, Invitation, QualityPreset, VideoCodec, Viewer, ViewerConfig,
    ViewerReason, ViewerState, ViewerStatus,
};
use pb_mapper_client::sdk::{Client, ConnectOptions};
use std::time::{Duration, Instant};

/// How often [`Supervisor::ended`] reads the viewer's status.
const POLL: Duration = Duration::from_millis(250);
/// The least time between two requests for control the host has not
/// granted yet.
const CONTROL_RETRY: Duration = Duration::from_secs(1);

/// When a viewer connects again. The default is the app's own policy.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// The wait before each retry of a run of failures, the last repeating:
    /// 1, 2, 4, 8 and 15 s.
    pub backoff: Vec<Duration>,
    /// A run of failures that lasted this long gives up: 120 s, time for a
    /// host that lost its relay, or restarts, to register again.
    pub window: Duration,
    /// `protocol` stops retried in a row before giving up, 3: one a
    /// version mismatch or a parse failure causes would loop.
    pub protocol_limit: u32,
    /// This long without video, while streaming, not paused and with a
    /// decoder that has not failed, is a stall, which ends a session as a
    /// `timeout` would: 5 s.
    pub stall: Duration,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            backoff: [1, 2, 4, 8, 15].map(Duration::from_secs).to_vec(),
            window: Duration::from_secs(120),
            protocol_limit: 3,
            stall: Duration::from_secs(5),
        }
    }
}

/// Connect again, after `delay`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    pub delay: Duration,
    /// Without offering H.265, which this device failed to decode.
    pub drop_h265: bool,
}

/// The policy, pure: what to do after a viewer ended or a connect failed.
/// Stops that come back by themselves retry with backoff: `transport`,
/// `timeout`, `capture_failed`, `restarting` (a host switching display), a
/// stall, and `protocol` a few times in a row; so do a lost path and a busy
/// host at connect. A decoder failure on H.265 connects again at once on
/// H.264, once. Anything else (the host stopped, ended the session or
/// revoked the invitation, this end stopped) gives up.
#[derive(Clone, Debug)]
pub struct Reconnect {
    policy: RetryPolicy,
    /// Retries in this run of failures.
    attempts: u32,
    /// `protocol` stops in a row.
    protocol_stops: u32,
    /// When this run of failures began.
    failing_since: Option<Instant>,
    /// H.265 was dropped; a decoder failure now ends the run.
    h265_dropped: bool,
}
impl Reconnect {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            attempts: 0,
            protocol_stops: 0,
            failing_since: None,
            h265_dropped: false,
        }
    }
    /// A session is live, streaming with a picture shown: a later failure
    /// starts a new run, from the first backoff step.
    pub fn connected(&mut self) {
        self.attempts = 0;
        self.protocol_stops = 0;
        self.failing_since = None;
    }
    /// `status` has had no video for the policy's `stall` while it was due.
    pub fn stalled(&self, status: &ViewerStatus) -> bool {
        status.state == ViewerState::Streaming
            && !status.paused
            && status.decoder.state != DecoderState::Failed
            && status
                .last_video_age_ms
                .is_some_and(|age| Duration::from_millis(age) >= self.policy.stall)
    }
    /// After a viewer ended at `now`, stopped or [stalled](Self::stalled),
    /// as its last status says: when to connect again, or `None` to give
    /// up. A status that did neither gives `None`.
    pub fn after(&mut self, ended: &ViewerStatus, now: Instant) -> Option<Retry> {
        if self.stalled(ended) {
            return self.backoff(now);
        }
        if ended.state != ViewerState::Stopped {
            return None;
        }
        match ended.reason? {
            ViewerReason::Protocol => {
                self.protocol_stops += 1;
                if self.protocol_stops > self.policy.protocol_limit {
                    return None;
                }
                self.backoff(now)
            }
            ViewerReason::Transport
            | ViewerReason::Timeout
            | ViewerReason::CaptureFailed
            | ViewerReason::Restarting => self.backoff(now),
            ViewerReason::DecoderFailed
                if ended.stats.codec == VideoCodec::H265 && !self.h265_dropped =>
            {
                self.h265_dropped = true;
                self.connected();
                Some(Retry {
                    delay: Duration::ZERO,
                    drop_h265: true,
                })
            }
            // H.264, or H.265 already dropped.
            ViewerReason::DecoderFailed
            | ViewerReason::HostStopped
            | ViewerReason::HostEnded
            | ViewerReason::Revoked
            | ViewerReason::SecureDesktop
            | ViewerReason::HelperUnavailable
            | ViewerReason::DisplayLost
            | ViewerReason::InputFailed
            | ViewerReason::ControlDenied
            | ViewerReason::InputCongestion
            | ViewerReason::StalePresentation
            | ViewerReason::DisplayChanging => None,
        }
    }
    /// After a connect failed at `now`: when to try again, or `None` to give
    /// up. Only a failed path and a busy host are worth another try.
    pub fn refused(&mut self, failure: &ConnectError, now: Instant) -> Option<Retry> {
        match failure {
            ConnectError::Transport(_) | ConnectError::Busy => self.backoff(now),
            ConnectError::IdentityChanged
            | ConnectError::VersionMismatch
            | ConnectError::Denied
            | ConnectError::InvalidRequest
            | ConnectError::Cancelled
            | ConnectError::Protocol(_) => None,
        }
    }
    fn backoff(&mut self, now: Instant) -> Option<Retry> {
        let since = *self.failing_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= self.policy.window {
            return None;
        }
        let step = usize::try_from(self.attempts).unwrap_or(usize::MAX);
        self.attempts = self.attempts.saturating_add(1);
        let backoff = &self.policy.backoff;
        Some(Retry {
            delay: backoff
                .get(step)
                .or(backoff.last())
                .copied()
                .unwrap_or_default(),
            drop_h265: false,
        })
    }
}

/// What the next [`Supervisor::next`] does.
#[derive(Clone, Copy, Debug)]
enum Next {
    Now,
    After(Duration),
    Done,
}

/// A viewer kept connected by a [`Reconnect`] policy. It connects, waits for
/// each viewer to end or stall, and connects again as the policy says,
/// keeping the control choice and the preset; after a decoder failure on
/// H.265 it no longer offers H.265.
pub struct Supervisor {
    client: Client,
    invitation: Invitation,
    options: ConnectOptions,
    config: ViewerConfig,
    reconnect: Reconnect,
    next: Next,
    /// Control is wanted: the choice the next viewer is handed.
    control: bool,
}
impl Supervisor {
    /// A supervisor of viewers of `invitation`, under the default policy.
    pub fn new(
        client: Client,
        invitation: Invitation,
        options: ConnectOptions,
        config: ViewerConfig,
    ) -> Self {
        Self {
            client,
            invitation,
            options,
            config,
            reconnect: Reconnect::new(RetryPolicy::default()),
            next: Next::Now,
            control: false,
        }
    }
    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.reconnect = Reconnect::new(policy);
        self
    }
    /// The next attempt, at once the first time, then after the delay the
    /// policy gave for how the last viewer ended or the last attempt failed.
    /// A failed attempt is returned too, and the policy says whether another
    /// follows; `None` once it gave up. Give each viewer to [`Self::ended`]
    /// before asking for the next.
    pub async fn next(&mut self) -> Option<std::result::Result<Viewer, ConnectError>> {
        match self.next {
            Next::Done => return None,
            Next::After(delay) => tokio::time::sleep(delay).await,
            Next::Now => {}
        }
        self.next = Next::Now;
        let attempt = Viewer::connect(
            &self.client,
            &self.invitation,
            self.options,
            self.config.clone(),
        )
        .await;
        if let Err(failure) = &attempt {
            let retry = self.reconnect.refused(failure, Instant::now());
            self.schedule(retry);
        }
        Some(attempt)
    }
    /// Resolves with `viewer`'s last status once it stopped or stalled; it
    /// is stopped on a stall. Meanwhile it follows the session: a live
    /// picture starts a new run of failures, and the preset and the control
    /// choice follow the viewer's.
    ///
    /// Control is the app's to choose ([`ViewerStatus::control_wanted`]):
    /// the remembered choice goes to a viewer the app has not asked yet, once
    /// its picture is current, and the supervisor never asks against what the
    /// app asked. Control the app wants but lost on the host's side (a pause,
    /// input congestion, a refusal) is asked for again, at most once a second,
    /// until the host's input fails or the host allows no control, as in the
    /// app; holding control again resumes that. The next viewer starts with
    /// the app's choice, unless those requests had stopped.
    pub async fn ended(&mut self, viewer: &Viewer) -> ViewerStatus {
        let mut ticks = tokio::time::interval(POLL);
        let mut handed = false;
        // Control lost on the host's side is asked for again.
        let mut again = true;
        let mut asked: Option<Instant> = None;
        loop {
            ticks.tick().await;
            let status = viewer.status();
            if status.controlling {
                again = true;
            } else if status.reason == Some(ViewerReason::InputFailed)
                || (status.fresh && !status.control_allowed)
            {
                again = false;
            }
            if !handed && status.control_wanted.is_none() && status.fresh {
                handed = true;
                asked = Some(Instant::now());
                if self.control && again && viewer.request_control(true).is_err() {
                    // Stopped meanwhile: the choice is kept for the next one.
                    handed = false;
                }
            }
            self.control = status.control_wanted.unwrap_or(self.control) && again;
            let stalled = self.reconnect.stalled(&status);
            if stalled || status.state == ViewerState::Stopped {
                if stalled {
                    let _ = viewer.stop().await;
                }
                let retry = self.reconnect.after(&status, Instant::now());
                self.schedule(retry);
                return status;
            }
            self.config.quality = status.quality;
            if status.state == ViewerState::Streaming && status.presented.is_some() {
                self.reconnect.connected();
            }
            if again
                && status.control_wanted == Some(true)
                && status.fresh
                && !status.controlling
                && asked.is_none_or(|at| at.elapsed() >= CONTROL_RETRY)
            {
                asked = Some(Instant::now());
                let _ = viewer.request_control(true);
            }
        }
    }
    /// The control choice the next viewer starts with, asked for once its
    /// picture is current unless the app asked that viewer first. During
    /// [`Self::ended`] the choice follows the app's instead: call
    /// [`Viewer::request_control`] there.
    pub fn remember_control(&mut self, enabled: bool) {
        self.control = enabled;
    }
    /// The preset the next viewer starts with; [`Self::ended`] keeps it in
    /// step with [`Viewer::set_quality`] by itself.
    pub fn remember_quality(&mut self, preset: QualityPreset) {
        self.config.quality = preset;
    }
    fn schedule(&mut self, retry: Option<Retry>) {
        self.next = match retry {
            None => Next::Done,
            Some(retry) => {
                if retry.drop_h265 {
                    self.config
                        .codecs
                        .retain(|codec| *codec != VideoCodec::H265);
                }
                Next::After(retry.delay)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DecoderInfo, DesktopKind, ViewerStats,
        test_util::{present, status_until},
    };
    use std::{io, sync::Arc};

    /// What happened, at a time in seconds.
    enum Event {
        Stopped(Option<ViewerReason>, VideoCodec),
        Stall {
            paused: bool,
            decoder: DecoderState,
        },
        /// The host settling after a display mode change, its capture away.
        Changing,
        Live,
        Refused(ConnectError),
    }
    /// Steps of one case: when, what, and the answer expected.
    type Steps = Vec<(u64, Event, Option<Retry>)>;

    fn status(state: ViewerState, reason: Option<ViewerReason>, codec: VideoCodec) -> ViewerStatus {
        ViewerStatus {
            state,
            stage: None,
            reason,
            message: String::new(),
            control_allowed: false,
            controlling: false,
            control_wanted: None,
            fresh: false,
            paused: false,
            generation: 1,
            display: None,
            desktop: DesktopKind::Ordinary,
            video: None,
            presented: None,
            last_video_age_ms: None,
            quality: QualityPreset::Balanced,
            capabilities: Vec::new(),
            modes: None,
            host_os: None,
            notice: None,
            decoder: DecoderInfo::default(),
            stats: ViewerStats {
                codec,
                ..ViewerStats::default()
            },
        }
    }

    #[test]
    fn the_policy_retries_what_comes_back_and_gives_up_on_the_rest() {
        use Event::*;
        use VideoCodec::{H264, H265};
        use ViewerReason as R;
        let retry = |secs| {
            Some(Retry {
                delay: Duration::from_secs(secs),
                drop_h265: false,
            })
        };
        let transport = || Stopped(Some(R::Transport), H264);
        let cases: Vec<(&str, Steps)> = vec![
            (
                "backoff, the last step repeating, until the window closes",
                vec![
                    (0, transport(), retry(1)),
                    (1, Stopped(Some(R::Timeout), H264), retry(2)),
                    (3, Stopped(Some(R::Restarting), H264), retry(4)),
                    (7, Stopped(Some(R::CaptureFailed), H264), retry(8)),
                    (15, transport(), retry(15)),
                    (30, transport(), retry(15)),
                    (120, transport(), None),
                ],
            ),
            (
                "a live picture starts a new run",
                vec![
                    (0, transport(), retry(1)),
                    (1, transport(), retry(2)),
                    (100, Live, None),
                    (200, transport(), retry(1)),
                ],
            ),
            (
                "protocol stops, a few in a row",
                vec![
                    (0, Stopped(Some(R::Protocol), H264), retry(1)),
                    (1, Stopped(Some(R::Protocol), H264), retry(2)),
                    (3, Stopped(Some(R::Protocol), H264), retry(4)),
                    (7, Stopped(Some(R::Protocol), H264), None),
                ],
            ),
            (
                "H.265 that cannot be decoded falls back to H.264 once",
                vec![
                    (0, transport(), retry(1)),
                    (
                        1,
                        Stopped(Some(R::DecoderFailed), H265),
                        Some(Retry {
                            delay: Duration::ZERO,
                            drop_h265: true,
                        }),
                    ),
                    // A new run: the first step again.
                    (2, transport(), retry(1)),
                    (3, Stopped(Some(R::DecoderFailed), H264), None),
                ],
            ),
            (
                "a stall is an end, unless paused, the decoder failed or the display is changing",
                vec![
                    (
                        0,
                        Stall {
                            paused: true,
                            decoder: DecoderState::Ok,
                        },
                        None,
                    ),
                    (
                        0,
                        Stall {
                            paused: false,
                            decoder: DecoderState::Failed,
                        },
                        None,
                    ),
                    (
                        0,
                        Stall {
                            paused: false,
                            decoder: DecoderState::Recovering,
                        },
                        retry(1),
                    ),
                    (0, Changing, None),
                ],
            ),
            (
                "what does not come back by itself gives up",
                vec![
                    (0, Stopped(Some(R::HostStopped), H264), None),
                    (0, Stopped(Some(R::HostEnded), H264), None),
                    (0, Stopped(Some(R::Revoked), H264), None),
                    (0, Stopped(None, H264), None),
                    (0, Refused(ConnectError::Denied), None),
                    (0, Refused(ConnectError::IdentityChanged), None),
                    (0, Refused(ConnectError::VersionMismatch), None),
                ],
            ),
            (
                "a lost path or a busy host at connect retries",
                vec![
                    (
                        0,
                        Refused(ConnectError::Transport(io::ErrorKind::TimedOut.into())),
                        retry(1),
                    ),
                    (1, Refused(ConnectError::Busy), retry(2)),
                ],
            ),
        ];
        let start = Instant::now();
        for (case, steps) in cases {
            let mut policy = Reconnect::new(RetryPolicy::default());
            for (at, event, expected) in steps {
                let now = start + Duration::from_secs(at);
                let got = match event {
                    Stopped(reason, codec) => {
                        policy.after(&status(ViewerState::Stopped, reason, codec), now)
                    }
                    Stall { paused, decoder } => {
                        let mut stalled = status(ViewerState::Streaming, None, VideoCodec::H264);
                        stalled.paused = paused;
                        stalled.decoder.state = decoder;
                        stalled.last_video_age_ms = Some(5_000);
                        policy.after(&stalled, now)
                    }
                    Changing => {
                        let mut changing = status(
                            ViewerState::CaptureUnavailable,
                            Some(R::DisplayChanging),
                            VideoCodec::H264,
                        );
                        changing.last_video_age_ms = Some(10_000);
                        assert!(!policy.stalled(&changing), "{case}");
                        policy.after(&changing, now)
                    }
                    Live => {
                        policy.connected();
                        None
                    }
                    Refused(failure) => policy.refused(&failure, now),
                };
                assert_eq!(got, expected, "{case}, at {at} s");
            }
        }
    }

    /// A JPEG desktop whose input fails when `failing`, as an injection the
    /// OS refuses does.
    struct Screen {
        failing: bool,
    }
    impl crate::Backend for Screen {
        fn display(&mut self) -> crate::Result<crate::Display> {
            Ok(crate::Display {
                id: 1,
                name: "supervised".into(),
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            })
        }
        fn capture(&mut self) -> crate::Result<Vec<u8>> {
            Ok(vec![1; 64])
        }
        fn input(&mut self, _: &crate::Input) -> crate::Result<()> {
            if self.failing {
                return Err(io::Error::other("input refused"));
            }
            Ok(())
        }
        fn release(&mut self) {}
    }
    /// Grants control after 600 ms, more than two polls.
    struct Unhurried;
    impl crate::ControlAdmission for Unhurried {
        fn acquire(&self) -> crate::Result<Box<dyn crate::ControlLease>> {
            std::thread::sleep(Duration::from_millis(600));
            Ok(Box::new(()))
        }
    }

    /// A relay, a host allowing control, a supervisor remembering control,
    /// and its first viewer, not yet shown.
    async fn supervised(
        name: &str,
        failing: bool,
    ) -> (pb_mapper_testkit::Relay, crate::Host, Supervisor, Viewer) {
        let relay = pb_mapper_testkit::Relay::start(name).await;
        let client = Client::from_credential(
            relay.addr().to_string(),
            pb_mapper_testkit::admin_credential(),
            true,
            None,
        );
        let mut options = crate::HostOptions::new("desktop:supervised");
        options.allow_direct = false;
        options.allow_control = true;
        let mut builder = crate::Host::builder(options);
        if failing {
            // Thread C takes its time over a grant, so the host's report of
            // the failure stays the viewer's for several polls after a
            // request: the window a supervisor must not act in.
            builder = builder.admission(Arc::new(Unhurried));
        }
        let host = builder
            .backend(move || Ok(Screen { failing }))
            .start(&client)
            .await
            .unwrap();
        let mut supervisor = Supervisor::new(
            client,
            host.invitation().clone(),
            ConnectOptions {
                path: pb_mapper_client::sdk::PathPolicy::RelayOnly,
                ..Default::default()
            },
            ViewerConfig::new(vec![VideoCodec::Jpeg]),
        );
        supervisor.remember_control(true);
        let viewer = supervisor.next().await.unwrap().unwrap();
        (relay, host, supervisor, viewer)
    }

    /// Longer than a poll and the pause between two requests together.
    const SETTLED: Duration = Duration::from_millis(250 + 2 * 1000);

    /// The user's "view only" is theirs: the supervisor hands control to a
    /// new viewer, never takes it back by itself, and the next viewer
    /// would start without it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn control_follows_what_the_viewer_last_asked() {
        let (_relay, host, mut supervisor, viewer) = supervised("desktop-supervisor", false).await;
        let renderer = present(&viewer, Arc::default());
        let script = async {
            status_until(&viewer, |status| status.controlling).await;
            viewer.request_control(false).unwrap();
            tokio::time::sleep(SETTLED).await;
            assert!(!viewer.status().controlling, "control was taken back");
            host.stop().await.unwrap();
        };
        let (ended, ()) = tokio::join!(supervisor.ended(&viewer), script);
        assert_eq!(ended.reason, Some(ViewerReason::HostStopped));
        assert!(!supervisor.control);
        assert!(supervisor.next().await.is_none());
        renderer.join().unwrap();
    }

    /// A choice the app made before the first picture is not overridden by
    /// the remembered one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_view_only_choice_before_the_picture_stands() {
        let (_relay, host, mut supervisor, viewer) =
            supervised("desktop-supervisor-early", false).await;
        viewer.request_control(false).unwrap();
        let renderer = present(&viewer, Arc::default());
        let script = async {
            status_until(&viewer, |status| status.fresh).await;
            tokio::time::sleep(SETTLED).await;
            let status = viewer.status();
            assert!(!status.controlling && status.control_wanted == Some(false));
            host.stop().await.unwrap();
        };
        let (_, ()) = tokio::join!(supervisor.ended(&viewer), script);
        assert!(!supervisor.control);
        renderer.join().unwrap();
    }

    /// After the host's input failed, the supervisor asks for control no
    /// more, as the app does, but never undoes the user's own request: one
    /// made while the host still reports the failure is granted and kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_input_failure_stops_automatic_requests_not_the_users() {
        let (_relay, host, mut supervisor, viewer) =
            supervised("desktop-supervisor-input", true).await;
        let renderer = present(&viewer, Arc::default());
        let script = async {
            let status = status_until(&viewer, |status| status.controlling && status.fresh).await;
            let scroll = crate::Input::Wheel {
                horizontal: 0,
                vertical: 1,
            };
            viewer.input(status.generation, &[scroll]).unwrap();
            status_until(&viewer, |status| {
                !status.controlling && status.reason == Some(ViewerReason::InputFailed)
            })
            .await;
            tokio::time::sleep(SETTLED).await;
            assert!(!viewer.status().controlling, "asked again by itself");
            viewer.request_control(true).unwrap();
            status_until(&viewer, |status| status.controlling).await;
            tokio::time::sleep(SETTLED).await;
            let status = viewer.status();
            assert!(status.controlling && status.control_wanted == Some(true));
            host.stop().await.unwrap();
        };
        let (_, ()) = tokio::join!(supervisor.ended(&viewer), script);
        renderer.join().unwrap();
    }
}

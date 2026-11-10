//! What the in-process desktop examples share: a relay in this process, a
//! synthetic desktop an embedder could have written, and a renderer thread.
//! Each example uses part of it.

use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use pb_mapper::desktop::{
    Backend, BackendCaps, Description, DesktopContext, Display, EncodedFrame, Input, Mode,
    ModeList, Pull, Result, VideoCodec, VideoFormat, VideoRate, Viewer,
};
use pb_mapper::{Client, ClientConfig};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// A relay on loopback, with direct coordination on the same port, as
/// `pb-mapper server` runs one, under a key made for this run.
pub struct LocalRelay {
    pub addr: SocketAddr,
    pub key: String,
    shutdown: CancellationToken,
    state_dir: PathBuf,
}
impl LocalRelay {
    pub async fn start(name: &str) -> std::result::Result<Self, Box<dyn Error>> {
        let key = pb_mapper_auth::generate_admin_key();
        let state_dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let auth = pb_mapper_auth::AuthRuntime::start(
            key.as_bytes().try_into()?,
            pb_mapper_auth::AuthConfig {
                state_dir: state_dir.clone(),
                ..Default::default()
            },
        )
        .await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        tokio::spawn(pb_mapper_server::run_server_on_listener_with_direct(
            listener,
            shutdown.clone(),
            None,
            false,
            auth,
            Some(pb_mapper_server::DirectServerConfig {
                listen: addr,
                advertise: SocketAddr::from(([0, 0, 0, 0], 0)),
            }),
        ));
        Ok(Self {
            addr,
            key,
            shutdown,
            state_dir,
        })
    }
    pub fn client(&self) -> pb_mapper::Result<Client> {
        Client::new(ClientConfig {
            server: self.addr.to_string(),
            credential: self.key.clone(),
            keep_alive: true,
            namespace: None,
        })
    }
    pub fn stop(self) {
        self.shutdown.cancel();
        let _ = std::fs::remove_dir_all(self.state_dir);
    }
}

/// The synthetic desktop's modes, largest first; it starts at the first.
pub const MODES: [Mode; 3] = [
    Mode {
        width: 1280,
        height: 720,
    },
    Mode {
        width: 1024,
        height: 576,
    },
    Mode {
        width: 800,
        height: 450,
    },
];

/// What the synthetic desktop shares with the example driving it.
pub struct Panel {
    /// Every input the host applied, in order.
    pub inputs: Mutex<Vec<Input>>,
    /// Times the host released held input.
    pub releases: AtomicU32,
    /// While set, capture yields nothing, as a capture that hangs would.
    pub frozen: AtomicBool,
    mode: Mutex<Mode>,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            inputs: Mutex::default(),
            releases: AtomicU32::new(0),
            frozen: AtomicBool::new(false),
            mode: Mutex::new(MODES[0]),
        }
    }
}
impl Panel {
    #[allow(dead_code, reason = "desktop_supervised never reads the inputs")]
    pub fn inputs(&self) -> Vec<Input> {
        self.inputs
            .lock()
            .map(|inputs| inputs.clone())
            .unwrap_or_default()
    }
    pub fn mode(&self) -> Mode {
        self.mode.lock().map_or(MODES[0], |mode| *mode)
    }
    fn set_mode(&self, mode: Mode) {
        if let Ok(mut current) = self.mode.lock() {
            *current = mode;
        }
    }
}

/// An embedder's `Backend`: a display of the current mode whose "encoder"
/// emits one small Annex B access unit per capture, an IDR when asked, and
/// which records the input it is given instead of injecting it. Nothing
/// here touches the operating system.
pub struct Synthetic {
    panel: Arc<Panel>,
    h265: bool,
    frames: u64,
}
impl Synthetic {
    /// `h265`: the encoder also makes H.265, for viewers that decode it.
    pub fn new(panel: Arc<Panel>, h265: bool) -> Self {
        Self {
            panel,
            h265,
            frames: 0,
        }
    }
}
impl Backend for Synthetic {
    fn display(&mut self) -> Result<Display> {
        let mode = self.panel.mode();
        Ok(Display {
            id: 1,
            name: "synthetic".into(),
            x: 0,
            y: 0,
            width: mode.width,
            height: mode.height,
        })
    }
    fn describe(&mut self, _probe_h265: bool) -> Result<Description> {
        let mut caps = BackendCaps::default();
        caps.h265 = self.h265;
        caps.display_modes = true;
        Ok(Description {
            display: self.display()?,
            caps,
        })
    }
    fn capture(&mut self) -> Result<Vec<u8>> {
        Err(std::io::Error::other(
            "this desktop sends H.264 or H.265 only",
        ))
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        _rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        if self.panel.frozen.load(Ordering::Acquire) {
            return Ok(None);
        }
        self.frames += 1;
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec,
                width: expected.display.width,
                height: expected.display.height,
                keyframe,
                pts_us: self.frames * 16_667,
                ..VideoFormat::default()
            },
            bytes: access_unit(codec, keyframe, self.frames),
        }))
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        if let Ok(mut inputs) = self.panel.inputs.lock() {
            inputs.push(input.clone());
        }
        Ok(())
    }
    fn release(&mut self) {
        self.panel.releases.fetch_add(1, Ordering::AcqRel);
    }
    fn display_modes(&mut self, _: &DesktopContext) -> Result<Option<ModeList>> {
        Ok(Some(ModeList {
            current: self.panel.mode(),
            original: MODES[0],
            supported: MODES.to_vec(),
        }))
    }
    fn set_display_mode(&mut self, _: &DesktopContext, mode: Mode) -> Result<()> {
        if !MODES.contains(&mode) {
            return Err(std::io::Error::other("not a mode of this display"));
        }
        self.panel.set_mode(mode);
        Ok(())
    }
    fn restore_display_mode(&mut self) -> Result<bool> {
        let changed = self.panel.mode() != MODES[0];
        self.panel.set_mode(MODES[0]);
        Ok(changed)
    }
}

/// One access unit of a picture that changes every frame: a start code, an
/// IDR or a non-IDR slice header for `codec`, and the frame number. No
/// decoder would show it; the examples never decode.
fn access_unit(codec: VideoCodec, keyframe: bool, frame: u64) -> Vec<u8> {
    let header: &[u8] = match (codec, keyframe) {
        (VideoCodec::H265, true) => &[0x26, 0x01],
        (VideoCodec::H265, false) => &[0x02, 0x01],
        (_, true) => &[0x65],
        (_, false) => &[0x41],
    };
    [&[0, 0, 0, 1], header, &frame.to_be_bytes()].concat()
}

/// A renderer on its own thread, as a native one runs: it takes each unit,
/// "decodes" the codecs `decodes` accepts and presents them, and reports a
/// decoder error for the rest. It returns how many it presented once the
/// session's video ends.
pub fn render(viewer: &Viewer, decodes: fn(VideoCodec) -> bool) -> JoinHandle<u64> {
    let consumer = viewer.video_sink().attach();
    std::thread::spawn(move || {
        let Ok(consumer) = consumer else {
            return 0;
        };
        let mut capacity = 1 << 20;
        let mut presented = 0;
        loop {
            match consumer.next(Duration::from_millis(100), capacity) {
                Pull::Unit(unit) => {
                    // After a discontinuity the sink starts at an IDR; a
                    // real decoder flushes and restarts there.
                    if !decodes(unit.codec) {
                        consumer.failed("this renderer has no decoder for the codec");
                        continue;
                    }
                    if consumer.presented(unit.generation, unit.frame_id) {
                        presented += 1;
                    }
                }
                Pull::NeedCapacity(needed) => capacity = needed,
                Pull::Timeout | Pull::Paused => {}
                Pull::Closed => return presented,
            }
        }
    })
}

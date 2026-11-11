//! A safe host for checking the real viewer apps on a screen: a relay in this
//! process and a host whose desktop is a recorded H.264 clip, never this
//! computer's screen, keyboard or mouse.
//!
//! ```text
//! ffmpeg -f lavfi -i "color=c=0x202020:s=1280x720:r=30:d=4" -c:v libx264 \
//!   -profile:v baseline -pix_fmt yuv420p -bf 0 \
//!   -x264-params keyint=1:min-keyint=1:scenecut=0:repeat-headers=1:aud=1 \
//!   -f h264 clip.h264
//! cargo run -p pb-mapper --features desktop --example desktop_ui_host -- \
//!   --clip clip.h264 --dir target/desktop-ui [--no-shapes] [--macos] [--relay-only]
//! node scripts/test/desktop-bootstrap.cjs target/desktop-ui
//! ```
//!
//! The clip is replayed in a loop, one access unit (split at each access unit
//! delimiter) per capture; a keyframe request skips to the clip's next IDR,
//! so each IDR must carry its parameter sets (`repeat-headers`). An all-IDR
//! clip (`keyint=1`, as above) meets every request at once; a slow viewer
//! takes a longer GOP better. The host's display is the clip's size, read
//! from the `--size` option (default 1280x720).
//!
//! The pointer's shape follows where the viewer puts it, in five vertical
//! bands a fifth of the display wide: arrow, I-beam, the top-left to
//! bottom-right resize arrow, hand, and a custom checkered disc. Every input
//! the host applies is printed as one `host_input` line, every shape it
//! reports as `host_shape`: that log is the evidence. `--no-shapes` hosts
//! without the `cursor_shape` capability, as a 0.11 host does; `--macos`
//! names macOS as the host's OS (`os_macos`), so a viewer offers the Apple
//! modifier swap. The host also lists `physical_keys`. `--relay-only` keeps
//! the host off direct paths and tells the viewer to stay on the relay, which
//! listens on loopback only. `--view-only` hosts without control; `--wander`
//! moves the host's own pointer across the bands, three seconds in each, as
//! someone at the host would. `--fps N` sends at most N units a second
//! (default 60), for a viewer as slow as an emulator.
//!
//! It writes `controller.json` into `--dir` for the viewer acceptance entry
//! (`ui/tool/desktop_viewer_acceptance.dart`, served by
//! `scripts/test/desktop-bootstrap.cjs`): the relay, its key for this run,
//! the invitation and `seconds`. It hosts until `--dir`/`stop` exists or
//! Ctrl+C; the relay and its key go with it.

#[expect(dead_code, reason = "this example takes only the relay")]
mod desktop_support;

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use desktop_support::LocalRelay;
use pb_mapper::desktop::{
    Backend, BackendCaps, CursorKind, CursorPosition, CursorShape, Description, DesktopContext,
    Display, EncodedFrame, Host, HostOptions, HostOs, Input, Result, VideoCodec, VideoFormat,
    VideoRate,
};

type Outcome<T = ()> = std::result::Result<T, Box<dyn Error>>;

/// The bands, left to right, and their shapes.
const BANDS: [CursorKind; 5] = [
    CursorKind::Arrow,
    CursorKind::Ibeam,
    CursorKind::ResizeNwse,
    CursorKind::Hand,
    CursorKind::Custom,
];

struct Options {
    clip: PathBuf,
    dir: PathBuf,
    width: u32,
    height: u32,
    shapes: bool,
    macos: bool,
    relay_only: bool,
    view_only: bool,
    wander: bool,
    /// Units a second at most: a slow viewer (an emulator) keeps up.
    fps: u32,
    seconds: u32,
}

fn options() -> Outcome<Options> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let (width, height) = match value("--size") {
        Some(size) => {
            let (w, h) = size.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
            (w.parse()?, h.parse()?)
        }
        None => (1280, 720),
    };
    Ok(Options {
        clip: value("--clip")
            .ok_or("--clip CLIP.h264 is required")?
            .into(),
        dir: value("--dir").ok_or("--dir DIRECTORY is required")?.into(),
        width,
        height,
        shapes: !flag("--no-shapes"),
        macos: flag("--macos"),
        relay_only: flag("--relay-only"),
        view_only: flag("--view-only"),
        wander: flag("--wander"),
        fps: value("--fps").map(|s| s.parse()).transpose()?.unwrap_or(60),
        seconds: value("--seconds")
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(900),
    })
}

#[tokio::main]
async fn main() -> Outcome {
    let options = options()?;
    let units = Arc::new(access_units(&std::fs::read(&options.clip)?));
    if units.is_empty() {
        return Err("the clip holds no access unit delimiter".into());
    }
    std::fs::create_dir_all(&options.dir)?;
    let _ = std::fs::remove_file(options.dir.join("stop"));
    let relay = LocalRelay::start("pb-mapper-desktop-ui-host").await?;
    let client = relay.client()?;

    let mut host_options = HostOptions::new("desktop:ui-acceptance");
    host_options.allow_control = !options.view_only;
    // Relay only: nothing listens beyond loopback, so no firewall asks.
    host_options.allow_direct = !options.relay_only;
    let (width, height) = (options.width, options.height);
    let (shapes, macos) = (options.shapes, options.macos);
    let interval = Duration::from_secs(1) / options.fps.clamp(1, 120);
    let clip = units.clone();
    let pointer: Pointer = Arc::new(Mutex::new(None));
    if options.wander {
        tokio::spawn(wander(pointer.clone(), width, height));
    }
    let shared = pointer.clone();
    let host = Host::builder(host_options)
        .backend(move || {
            Ok(Replay {
                units: clip.clone(),
                next: 0,
                interval,
                last: None,
                width,
                height,
                shapes,
                os: if macos {
                    HostOs::Macos
                } else {
                    HostOs::Windows
                },
                pointer: shared.clone(),
                shown: None,
            })
        })
        .start(&client)
        .await?;

    let controller = serde_json::json!({
        "server": relay.addr.to_string(),
        "credential": relay.key,
        "transport": "tcp",
        "invitation": host.invitation().encode(),
        "seconds": options.seconds,
        "relay_only": options.relay_only,
    });
    let staged = options.dir.join("controller.json.tmp");
    std::fs::write(&staged, serde_json::to_vec(&controller)?)?;
    std::fs::rename(&staged, options.dir.join("controller.json"))?;
    println!(
        "host_ready relay={} display={width}x{height} units={} cursor_shape={shapes} os={}",
        relay.addr,
        units.len(),
        if macos { "macos" } else { "windows" }
    );

    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut ticks = tokio::time::interval(Duration::from_millis(500));
    let mut last = String::new();
    loop {
        tokio::select! {
            biased;
            result = &mut interrupt => { result?; break; }
            _ = ticks.tick() => {
                if options.dir.join("stop").exists() {
                    break;
                }
                let status = host.status();
                let line = format!(
                    "host_state {:?} viewer={:?}",
                    status.state,
                    status.viewer.map(|v| (v.path, v.controlling))
                );
                if line != last {
                    println!("{line}");
                    last = line;
                }
            }
        }
    }
    host.stop().await?;
    let _ = std::fs::remove_file(options.dir.join("controller.json"));
    drop(relay);
    println!("host_stopped");
    Ok(())
}

/// Where the host's pointer is, in display pixels; `None` until it is put
/// somewhere.
type Pointer = Arc<Mutex<Option<(u32, u32)>>>;

/// The host's own pointer, as if someone at it moved it: three seconds in
/// each band, left to right, drifting up and down.
async fn wander(pointer: Pointer, width: u32, height: u32) {
    let started = std::time::Instant::now();
    let mut ticks = tokio::time::interval(Duration::from_millis(50));
    loop {
        ticks.tick().await;
        let t = started.elapsed().as_secs_f64();
        let band = (t / 3.0) as u32 % 5;
        let x = band * width / 5 + width / 10 + (t.sin() * f64::from(width) / 40.0) as u32;
        let y = height / 2 + (t.cos() * f64::from(height) / 8.0) as u32;
        if let Ok(mut pointer) = pointer.lock() {
            *pointer = Some((x.min(width - 1), y.min(height - 1)));
        }
    }
}

/// The clip's access units, each from its delimiter (NAL type 9) to the
/// next.
fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let starts: Vec<usize> = (0..stream.len().saturating_sub(3))
        .filter(|&at| stream[at..at + 3] == [0, 0, 1] && stream[at + 3] & 0x1f == 9)
        .map(|at| {
            if at > 0 && stream[at - 1] == 0 {
                at - 1
            } else {
                at
            }
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(stream.len());
            stream[start..end].to_vec()
        })
        .collect()
}

/// The unit holds an IDR slice (NAL type 5).
fn is_idr(unit: &[u8]) -> bool {
    unit.windows(4)
        .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1f == 5)
}

/// The replayed desktop: one unit per capture, the pointer where the
/// viewer last moved it, its shape by band, every input printed.
struct Replay {
    units: Arc<Vec<Vec<u8>>>,
    next: usize,
    interval: Duration,
    last: Option<Instant>,
    width: u32,
    height: u32,
    shapes: bool,
    os: HostOs,
    pointer: Pointer,
    /// The band whose shape was reported last.
    shown: Option<usize>,
}
impl Replay {
    fn pointer(&self) -> Option<(u32, u32)> {
        self.pointer.lock().ok().and_then(|pointer| *pointer)
    }
}
impl Backend for Replay {
    fn display(&mut self) -> Result<Display> {
        Ok(Display {
            id: 1,
            name: "replay".into(),
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        })
    }
    fn describe(&mut self, _probe_h265: bool) -> Result<Description> {
        let mut caps = BackendCaps::default();
        caps.os = Some(self.os);
        caps.physical_keys = true;
        caps.cursor_shapes = self.shapes;
        Ok(Description {
            display: self.display()?,
            caps,
        })
    }
    fn capture_video(
        &mut self,
        expected: &DesktopContext,
        codec: VideoCodec,
        keyframe: bool,
        _rate: VideoRate,
    ) -> Result<Option<EncodedFrame>> {
        if codec != VideoCodec::H264 {
            return Err(std::io::Error::other("this desktop replays H.264 only"));
        }
        let now = Instant::now();
        if self.last.is_some_and(|last| now - last < self.interval) {
            return Ok(None);
        }
        self.last = Some(now);
        // A keyframe asked for is the next IDR of the clip.
        let count = self.units.len();
        if keyframe {
            while !is_idr(&self.units[self.next % count]) {
                self.next += 1;
            }
        }
        let unit = self.units[self.next % count].clone();
        self.next += 1;
        let idr = is_idr(&unit);
        Ok(Some(EncodedFrame {
            format: VideoFormat {
                codec,
                width: expected.display.width,
                height: expected.display.height,
                keyframe: idr,
                pts_us: self.next as u64 * 33_333,
                ..VideoFormat::default()
            },
            bytes: unit,
        }))
    }
    fn cursor(&mut self, _: &DesktopContext) -> Result<Option<CursorPosition>> {
        Ok(self.pointer().map(|(x, y)| CursorPosition {
            x: x as i32,
            y: y as i32,
            visible: true,
        }))
    }
    fn cursor_shape(&mut self, _: &DesktopContext) -> Result<Option<CursorShape>> {
        let Some((x, y)) = self.pointer() else {
            return Ok(None);
        };
        let band = ((x * 5) / self.width).min(4) as usize;
        if self.shown == Some(band) {
            return Ok(None);
        }
        self.shown = Some(band);
        let kind = BANDS[band];
        println!("host_shape kind={kind:?} at={x},{y}");
        let (hotspot, rgba) = bitmap(kind);
        CursorShape::new(kind, 32, 32, hotspot, rgba).map(Some)
    }
    fn input(&mut self, input: &Input) -> Result<()> {
        match input {
            Input::Move { x, y } => {
                if let Ok(mut pointer) = self.pointer.lock() {
                    *pointer = Some((*x, *y));
                }
            }
            Input::PhysicalKey { usage, down } => {
                println!("host_input physical_key usage={usage:#04x} down={down}");
            }
            other => println!("host_input {other:?}"),
        }
        Ok(())
    }
    fn release(&mut self) {
        println!("host_release");
    }
}

/// A 32x32 straight RGBA bitmap for `kind` and its hotspot: plain shapes a
/// screenshot tells apart.
fn bitmap(kind: CursorKind) -> ((u32, u32), Vec<u8>) {
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    let mut rgba = vec![0u8; 32 * 32 * 4];
    let mut put = |x: i32, y: i32, colour: [u8; 4]| {
        if (0..32).contains(&x) && (0..32).contains(&y) {
            let at = (y as usize * 32 + x as usize) * 4;
            rgba[at..at + 4].copy_from_slice(&colour);
        }
    };
    let hotspot = match kind {
        CursorKind::Arrow => {
            for y in 0..22 {
                let edge = y * 3 / 5;
                for x in 0..=edge {
                    let border = x == 0 || x == edge || y == 21;
                    put(x, y, if border { BLACK } else { WHITE });
                }
            }
            (0, 0)
        }
        CursorKind::Ibeam => {
            for y in 4..28 {
                put(15, y, BLACK);
                put(16, y, BLACK);
            }
            for x in 11..21 {
                put(x, 4, BLACK);
                put(x, 27, BLACK);
            }
            (16, 16)
        }
        CursorKind::ResizeNwse => {
            for i in 3..29 {
                for t in -1..=1 {
                    put(i + t, i, BLACK);
                }
            }
            for i in 0..8 {
                put(3 + i, 3, BLACK);
                put(3, 3 + i, BLACK);
                put(28 - i, 28, BLACK);
                put(28, 28 - i, BLACK);
            }
            (16, 16)
        }
        CursorKind::Hand => {
            for y in 0..32 {
                for x in 0..32 {
                    let d = (x - 16) * (x - 16) + (y - 18) * (y - 18);
                    if d <= 100 {
                        put(x, y, if d >= 82 { BLACK } else { [255, 214, 0, 255] });
                    }
                }
            }
            for y in 2..10 {
                put(15, y, BLACK);
                put(16, y, BLACK);
            }
            (16, 2)
        }
        _ => {
            // Custom: a magenta and green checkered disc.
            for y in 0..32 {
                for x in 0..32 {
                    let d = (x - 16) * (x - 16) + (y - 16) * (y - 16);
                    if d <= 225 {
                        let magenta = ((x / 4) + (y / 4)) % 2 == 0;
                        put(
                            x,
                            y,
                            if magenta {
                                [255, 0, 255, 255]
                            } else {
                                [0, 200, 0, 255]
                            },
                        );
                    }
                }
            }
            (16, 16)
        }
    };
    (hotspot, rgba)
}

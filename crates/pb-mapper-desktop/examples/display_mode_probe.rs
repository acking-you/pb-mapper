//! The display-mode acceptance (contract §10.5 item 4) on the computer this
//! runs on. It CHANGES THIS COMPUTER'S DISPLAY MODE, dynamically (never in
//! the registry), and puts it back: run it only where the computer's owner
//! agreed, or in the VM of `scripts/test/windows-helper-acceptance.md`.
//!
//! ```text
//! display_mode_probe --list
//! display_mode_probe --restore [--state-dir DIR]
//! display_mode_probe <relay-config.json> [--fit WxH] [--state-dir DIR]
//!                    [--owner-change] [--fullscreen-check]
//! ```
//!
//! `--list` prints every display's mode and changes nothing. `--restore`
//! replays the probe's journal, then puts any display that does not show its
//! registry mode back to it.
//!
//! The configuration is the `deployed` example's (`server_address`,
//! `msg_header_key`, `relay_transport`): use a local relay with a throwaway
//! key. The host allows control, which a mode change needs; the probe sends
//! no input. Its journal lives in `--state-dir` (by default a directory in
//! the temporary directory), never in the app's. It records every display's
//! mode, then checks after each case that the display is back at it:
//!
//! 1. Fit local (`WxH`, 1920x1080 by default), then Original;
//! 2. Fit local, then the viewer disconnects;
//! 3. Fit local, then the host revokes control;
//! 4. Fit local on a host in a child process, which is killed; a host
//!    started again puts the display back from the journal.
//!
//! `--owner-change` adds a fifth: Fit local, then the probe changes the mode
//! itself as the owner would, and the viewer disconnects; the host must
//! leave the owner's mode. `--fullscreen-check` asks whether Windows puts a
//! `CDS_FULLSCREEN` change back when the process that made it exits
//! (contract R13). Neither is part of a run on a computer whose owner agreed
//! only to the five documented paths.
//!
//! At the end, or after any failure, every display is compared with what was
//! recorded and put back where it differs. The last line is
//! `display_mode_probe passed`, or the run fails with what went wrong.

#[cfg(not(windows))]
fn main() {
    eprintln!("display_mode_probe changes a Windows display's mode; it runs on Windows only");
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(probe::run())
}

#[cfg(windows)]
mod probe {
    use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
    use pb_mapper_desktop::{
        Host, HostOptions, Invitation, Mode, ModeTarget, Pull, VideoCodec, Viewer, ViewerConfig,
        ViewerStatus, displays, recover_display_modes,
    };
    use serde::Deserialize;
    use std::{
        io::{BufRead, BufReader},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use windows::{
        Win32::Graphics::Gdi::{
            CDS_FULLSCREEN, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
            DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
            ENUM_DISPLAY_SETTINGS_MODE, ENUM_REGISTRY_SETTINGS, EnumDisplaySettingsExW,
        },
        core::PCWSTR,
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    /// A host refuses another change this long after one.
    const SETTLE: Duration = Duration::from_millis(3500);

    #[derive(Deserialize)]
    struct Configuration {
        server_address: String,
        msg_header_key: String,
        #[serde(default)]
        relay_transport: RelayTransport,
    }

    /// One display's mode, as Windows reports it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Shown {
        device: String,
        width: u32,
        height: u32,
        hz: u32,
        orientation: u32,
        primary: bool,
    }
    impl Shown {
        fn size(&self) -> Mode {
            Mode {
                width: self.width,
                height: self.height,
            }
        }
    }
    impl std::fmt::Display for Shown {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{} {}x{} at {} Hz, orientation {}{}",
                self.device,
                self.width,
                self.height,
                self.hz,
                self.orientation * 90,
                if self.primary { ", primary" } else { "" }
            )
        }
    }

    pub async fn run() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let flag = |name: &str| args.iter().any(|arg| arg == name);
        let option = |name: &str| {
            args.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].clone())
        };
        if flag("--list") {
            return print("now", &every_display()?);
        }
        let state_dir = option("--state-dir")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("pb-mapper-display-mode-probe"));
        if flag("--restore") {
            println!(
                "journal entries replayed: {}",
                recover_display_modes(Some(&state_dir))?
            );
            for display in every_display()? {
                if read(&display.device, ENUM_REGISTRY_SETTINGS).is_some_and(|saved| {
                    (saved.width, saved.height, saved.hz)
                        != (display.width, display.height, display.hz)
                }) {
                    change(&display.device, None, CDS_TYPE(0))?;
                }
            }
            return print("now", &every_display()?);
        }
        if let Some(size) = option("--fullscreen-child") {
            return fullscreen_child(&option("--device").ok_or("device required")?, parse(&size)?);
        }
        let config_path = args.get(1).ok_or("configuration path required")?;
        let config: Configuration = serde_json::from_slice(&std::fs::read(config_path)?)?;
        let client = Client::new(ClientConfig {
            server: config.server_address,
            credential: config.msg_header_key,
            keep_alive: true,
            namespace: None,
        })?
        .with_relay_transport(config.relay_transport)?;
        if flag("--child-host") {
            return child_host(&client, &state_dir).await;
        }
        let fit = option("--fit")
            .map(|size| parse(&size))
            .transpose()?
            .unwrap_or(Mode {
                width: 1920,
                height: 1080,
            });

        let before = every_display()?;
        print("before", &before)?;
        let probe = Probe {
            client: &client,
            config_path,
            state_dir: &state_dir,
            before: &before,
        };
        let outcome = probe.cases(fit, flag("--owner-change")).await;
        let outcome = match outcome {
            Ok(()) if flag("--fullscreen-check") => probe.fullscreen_check(fit),
            other => other,
        };
        // Whatever happened, every display ends at its recorded mode.
        let put_back = probe.put_back();
        let after = every_display()?;
        print("after", &after)?;
        outcome?;
        put_back?;
        if after != before {
            return Err("a display is not back at its recorded mode".into());
        }
        println!("display_mode_probe passed");
        Ok(())
    }

    struct Probe<'a> {
        client: &'a Client,
        config_path: &'a str,
        state_dir: &'a Path,
        before: &'a [Shown],
    }
    impl Probe<'_> {
        async fn cases(&self, fit: Mode, owner_change: bool) -> Result<()> {
            let host = start_host(self.client, self.state_dir).await?;
            let viewer = Controlling::connect(self.client, host.invitation()).await?;
            let device = viewer.device()?;
            let own = self.recorded(&device)?;

            // 1: Fit local, then Original.
            let fit = viewer.change(ModeTarget::FitLocal(fit)).await?;
            tokio::time::sleep(SETTLE).await;
            viewer.change(ModeTarget::Original).await?;
            self.back(&own, "1. Fit local, then Original").await?;

            // 2: Fit local, then the viewer disconnects.
            tokio::time::sleep(SETTLE).await;
            viewer.change(ModeTarget::Size(fit)).await?;
            viewer.stop().await?;
            self.back(&own, "2. Fit local, then disconnect").await?;

            // 3: Fit local, then the host revokes control.
            let viewer = Controlling::connect(self.client, host.invitation()).await?;
            viewer.change(ModeTarget::Size(fit)).await?;
            host.set_control_allowed(false);
            self.back(&own, "3. Fit local, then revoke").await?;
            viewer.stop().await?;
            host.stop().await?;

            // 4: a host killed with the mode changed; the next one replays
            // the journal.
            let mut child = Command::new(std::env::current_exe()?)
                .args([self.config_path, "--child-host", "--state-dir"])
                .arg(self.state_dir)
                .stdout(Stdio::piped())
                .spawn()?;
            let mut line = String::new();
            BufReader::new(child.stdout.take().ok_or("no child output")?).read_line(&mut line)?;
            let invitation = Invitation::decode(line.trim())?;
            let killed = Controlling::connect(self.client, &invitation).await?;
            killed.change(ModeTarget::Size(fit)).await?;
            child.kill()?;
            child.wait()?;
            println!("   killed the host; {}", shown(&device)?);
            drop(killed);
            let restarted = start_host(self.client, self.state_dir).await?;
            self.back(&own, "4. a killed host, then a host started again")
                .await?;

            // 5: the owner changes the mode meanwhile: never fought.
            if owner_change {
                let viewer = Controlling::connect(self.client, restarted.invitation()).await?;
                viewer.change(ModeTarget::Size(fit)).await?;
                let owners = viewer
                    .status()
                    .modes
                    .and_then(|modes| {
                        modes
                            .supported
                            .into_iter()
                            .find(|mode| *mode != fit && *mode != own.size())
                    })
                    .ok_or("no third mode for the owner to pick")?;
                change(&device, Some(owners), CDS_TYPE(0))?;
                until("the owner's mode to show", || {
                    shown(&device).is_ok_and(|now| now.size() == owners)
                })
                .await?;
                viewer.stop().await?;
                tokio::time::sleep(Duration::from_secs(2)).await;
                let kept = shown(&device)?;
                if kept.size() != owners {
                    return Err(format!("the host fought the owner: {kept}").into());
                }
                println!("5. the owner's {}x{} stayed", owners.width, owners.height);
                change(&device, None, CDS_TYPE(0))?;
                self.back(&own, "   the probe put the registry mode back")
                    .await?;
            }
            restarted.stop().await?;
            Ok(())
        }

        fn recorded(&self, device: &str) -> Result<Shown> {
            Ok(self
                .before
                .iter()
                .find(|display| display.device == device)
                .ok_or("the viewer's display was not recorded")?
                .clone())
        }

        /// Wait until `own`'s display shows it again, and say so.
        async fn back(&self, own: &Shown, case: &str) -> Result<()> {
            until("the display back at its recorded mode", || {
                shown(&own.device).is_ok_and(|now| now == *own)
            })
            .await?;
            println!("{case}: back at {own}");
            Ok(())
        }

        /// The journal replayed, and any display not at its recorded mode
        /// put back to its registry mode, which no case writes.
        fn put_back(&self) -> Result<()> {
            let replayed = recover_display_modes(Some(self.state_dir))?;
            if replayed > 0 {
                println!("journal entries replayed at the end: {replayed}");
            }
            for display in every_display()? {
                if self.before.contains(&display) {
                    continue;
                }
                println!("putting {} back", display.device);
                change(&display.device, None, CDS_TYPE(0))?;
            }
            Ok(())
        }

        /// R13: does Windows put a `CDS_FULLSCREEN` change back when the
        /// process that made it exits? A child makes it and exits; this one
        /// looks.
        fn fullscreen_check(&self, fit: Mode) -> Result<()> {
            let own = self
                .before
                .iter()
                .find(|display| display.primary)
                .ok_or("no primary display")?;
            let status = Command::new(std::env::current_exe()?)
                .args([
                    "--fullscreen-child",
                    &format!("{}x{}", fit.width, fit.height),
                    "--device",
                    &own.device,
                ])
                .status()?;
            if !status.success() {
                return Err("the CDS_FULLSCREEN child failed".into());
            }
            std::thread::sleep(Duration::from_secs(2));
            let reverted = shown(&own.device)? == *own;
            println!("CDS_FULLSCREEN reverts when its process exits: {reverted}");
            Ok(())
        }
    }

    /// The child of case 4: a host whose invitation goes to stdout; it runs
    /// until it is killed.
    async fn child_host(client: &Client, state_dir: &Path) -> Result<()> {
        let host = start_host(client, state_dir).await?;
        println!("{}", host.invitation().encode());
        std::future::pending::<()>().await;
        Ok(())
    }

    async fn start_host(client: &Client, state_dir: &Path) -> Result<Host> {
        let mut options = HostOptions::new("desktop:display-mode-probe");
        options.allow_control = true;
        options.allow_direct = false;
        options.state_dir = Some(state_dir.to_path_buf());
        Ok(Host::start(client, options).await?)
    }

    /// A viewer that presents every unit at once and holds control; it
    /// sends no input.
    struct Controlling {
        viewer: Viewer,
        presenting: std::thread::JoinHandle<()>,
    }
    impl Controlling {
        async fn connect(client: &Client, invitation: &Invitation) -> Result<Self> {
            let options = ConnectOptions {
                path: PathPolicy::RelayOnly,
                ..Default::default()
            };
            let viewer = Viewer::connect(
                client,
                invitation,
                options,
                ViewerConfig::new(vec![VideoCodec::H264]),
            )
            .await
            .map_err(|failure| failure.to_string())?;
            let consumer = viewer.video_sink().attach()?;
            let presenting = std::thread::spawn(move || {
                loop {
                    match consumer.next(Duration::from_millis(50), 8 << 20) {
                        Pull::Unit(unit) => {
                            consumer.presented(unit.generation, unit.frame_id);
                        }
                        Pull::Closed => return,
                        _ => {}
                    }
                }
            });
            let this = Self { viewer, presenting };
            this.until("a fresh picture", |status| status.fresh).await?;
            this.viewer.request_control(true)?;
            this.until("control", |status| status.controlling).await?;
            this.until("the display's modes", |status| status.modes.is_some())
                .await?;
            Ok(this)
        }
        fn status(&self) -> ViewerStatus {
            self.viewer.status()
        }
        fn device(&self) -> Result<String> {
            Ok(self.status().display.ok_or("no display reported")?.name)
        }
        async fn until(&self, what: &str, condition: impl Fn(&ViewerStatus) -> bool) -> Result<()> {
            until(what, || condition(&self.viewer.status())).await
        }
        /// Ask for `target`, and wait until the viewer sees the mode it became.
        async fn change(&self, target: ModeTarget) -> Result<Mode> {
            let size = self
                .viewer
                .set_display_mode(target)
                .map_err(|failure| format!("{failure:?}"))?;
            self.until("the new size", |status| {
                status
                    .display
                    .as_ref()
                    .is_some_and(|d| (d.width, d.height) == (size.width, size.height))
                    && status.fresh
            })
            .await?;
            Ok(size)
        }
        async fn stop(self) -> Result<()> {
            self.viewer.stop().await?;
            drop(self.viewer);
            let _ = self.presenting.join();
            Ok(())
        }
    }

    fn print(label: &str, displays: &[Shown]) -> Result<()> {
        for display in displays {
            println!("{label}: {display}");
        }
        Ok(())
    }

    /// Every display's mode now.
    fn every_display() -> Result<Vec<Shown>> {
        displays()?
            .into_iter()
            .map(|display| shown(&display.name))
            .collect()
    }

    /// The mode `device` shows now.
    fn shown(device: &str) -> Result<Shown> {
        let display = displays()?
            .into_iter()
            .find(|display| display.name == device)
            .ok_or("the display is gone")?;
        let current = read(device, ENUM_CURRENT_SETTINGS).ok_or("the mode cannot be read")?;
        Ok(Shown {
            primary: (display.x, display.y) == (0, 0),
            ..current
        })
    }

    /// `device`'s mode at `index`: the current one, or the registry's.
    fn read(device: &str, index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<Shown> {
        let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
        let mut mode = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let found = unsafe {
            EnumDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                index,
                &mut mode,
                ENUM_DISPLAY_SETTINGS_FLAGS(0),
            )
        };
        // A display device's DEVMODE carries the display half of the union.
        let orientation = unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation.0 };
        found.as_bool().then(|| Shown {
            device: device.to_owned(),
            width: mode.dmPelsWidth,
            height: mode.dmPelsHeight,
            hz: mode.dmDisplayFrequency,
            orientation,
            primary: false,
        })
    }

    async fn until(what: &str, mut condition: impl FnMut() -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            if Instant::now() >= deadline {
                return Err(format!("timed out waiting for {what}").into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    fn parse(size: &str) -> Result<Mode> {
        let (width, height) = size.split_once('x').ok_or("sizes are WxH")?;
        Ok(Mode {
            width: width.parse()?,
            height: height.parse()?,
        })
    }

    /// `mode` on `device` with `flags`, or for `None` the registry mode.
    fn change(device: &str, mode: Option<Mode>, flags: CDS_TYPE) -> Result<()> {
        let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
        let devmode = mode.map(|mode| DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            dmFields: DM_PELSWIDTH | DM_PELSHEIGHT,
            dmPelsWidth: mode.width,
            dmPelsHeight: mode.height,
            ..Default::default()
        });
        let changed = unsafe {
            ChangeDisplaySettingsExW(
                PCWSTR(name.as_ptr()),
                devmode.as_ref().map(std::ptr::from_ref),
                None,
                flags,
                None,
            )
        };
        if changed != DISP_CHANGE_SUCCESSFUL {
            return Err(format!("the display refused the change ({})", changed.0).into());
        }
        Ok(())
    }

    fn fullscreen_child(device: &str, fit: Mode) -> Result<()> {
        change(device, Some(fit), CDS_FULLSCREEN)?;
        std::thread::sleep(Duration::from_secs(2));
        Ok(())
    }
}

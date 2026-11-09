//! The display-mode acceptance (contract §10.5 item 4) on the computer this
//! runs on. It CHANGES THIS COMPUTER'S DISPLAY MODE, dynamically, and puts it
//! back: run it only where the computer's owner agreed, or in the VM of
//! `scripts/test/windows-helper-acceptance.md`.
//!
//! ```text
//! display_mode_probe <relay-config.json> [--fit WxH] [--fullscreen-check]
//! ```
//!
//! The configuration is the `deployed` example's (`server_address`,
//! `msg_header_key`, `relay_transport`): use a local relay with a throwaway
//! key. The host has ordinary access and allows control, which a mode change
//! needs; the probe sends no input. In order, it:
//!
//! 1. fits `WxH` (1920x1080 by default) through a viewer, and waits until the
//!    viewer sees the display at that size;
//! 2. disconnects, and checks the display is back at its own mode;
//! 3. runs a host in a child process, fits `WxH` there, kills the child in
//!    the middle of the change, and checks that the next host's start puts
//!    the display back from the journal;
//! 4. fits `WxH` again, changes the mode itself to another offered size
//!    meanwhile, as the owner would, disconnects, and checks that the host
//!    left the owner's choice alone; then puts the registry mode back.
//!
//! `--fullscreen-check` then asks whether Windows reverts a `CDS_FULLSCREEN`
//! change when the process that made it exits (contract R13), in a child.
//! Every step prints one line; the last prints `display_mode_probe passed`.

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
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use windows::{
        Win32::Graphics::Gdi::{
            CDS_FULLSCREEN, CDS_TYPE, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
            DM_PELSHEIGHT, DM_PELSWIDTH,
        },
        core::PCWSTR,
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    #[derive(Deserialize)]
    struct Configuration {
        server_address: String,
        msg_header_key: String,
        #[serde(default)]
        relay_transport: RelayTransport,
    }

    pub async fn run() -> Result<()> {
        let args: Vec<String> = std::env::args().collect();
        let option = |name: &str| {
            args.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].clone())
        };
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
        if args.iter().any(|arg| arg == "--child-host") {
            return child_host(&client).await;
        }
        let fit = option("--fit")
            .map(|size| parse(&size))
            .transpose()?
            .unwrap_or(Mode {
                width: 1920,
                height: 1080,
            });

        // 1 and 2: fit, see it, and get the display back at the end.
        let host = start_host(&client).await?;
        let viewer = Controlling::connect(&client, host.invitation()).await?;
        let device = viewer.device()?;
        let own = shown(&device)?;
        println!("display {device} shows {}x{}", own.width, own.height);
        let fit = viewer.change(ModeTarget::FitLocal(fit)).await?;
        println!("1. fitted {}x{}; the viewer sees it", fit.width, fit.height);
        viewer.stop().await?;
        until("the display back at its own mode", || {
            shown(&device).ok() == Some(own)
        })
        .await?;
        println!(
            "2. disconnected; the display is back at {}x{}",
            own.width, own.height
        );
        host.stop().await?;

        // 3: a host killed mid-change, then a host started again.
        let mut child = Command::new(std::env::current_exe()?)
            .args([config_path.as_str(), "--child-host"])
            .stdout(Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take().ok_or("no child output")?).read_line(&mut line)?;
        let invitation = Invitation::decode(line.trim())?;
        let killed = Controlling::connect(&client, &invitation).await?;
        killed.change(ModeTarget::Size(fit)).await?;
        child.kill()?;
        child.wait()?;
        let left = shown(&device)?;
        println!(
            "3. killed the host mid-change; the display shows {}x{}",
            left.width, left.height
        );
        drop(killed);
        let restarted = start_host(&client).await?;
        until("the restarted host to put the display back", || {
            shown(&device).ok() == Some(own)
        })
        .await?;
        println!(
            "   started again; the display is back at {}x{}",
            own.width, own.height
        );

        // 4: the owner changes the mode meanwhile: never fought.
        let viewer = Controlling::connect(&client, restarted.invitation()).await?;
        viewer.change(ModeTarget::Size(fit)).await?;
        let owners = viewer
            .status()
            .modes
            .and_then(|modes| {
                modes
                    .supported
                    .into_iter()
                    .find(|mode| *mode != fit && *mode != own)
            })
            .ok_or("no third mode for the owner to pick")?;
        change(&device, Some(owners), CDS_TYPE(0))?;
        until("the owner's mode to show", || {
            shown(&device).ok() == Some(owners)
        })
        .await?;
        viewer.stop().await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let kept = shown(&device)?;
        if kept != owners {
            return Err(
                format!("the host fought the owner: {}x{}", kept.width, kept.height).into(),
            );
        }
        println!(
            "4. the owner's {}x{} stayed after the session ended",
            owners.width, owners.height
        );
        change(&device, None, CDS_TYPE(0))?;
        restarted.stop().await?;
        println!("   the registry mode is back");

        if args.iter().any(|arg| arg == "--fullscreen-check") {
            fullscreen_check(&device, fit, own)?;
        }
        // Nothing of this probe's is left to replay.
        println!(
            "journal entries restored at the end: {}",
            recover_display_modes(None)?
        );
        println!("display_mode_probe passed");
        Ok(())
    }

    /// The child of step 3: a host whose invitation goes to stdout; it runs
    /// until it is killed.
    async fn child_host(client: &Client) -> Result<()> {
        let host = start_host(client).await?;
        println!("{}", host.invitation().encode());
        std::future::pending::<()>().await;
        Ok(())
    }

    async fn start_host(client: &Client) -> Result<Host> {
        let mut options = HostOptions::new("desktop:display-mode-probe");
        options.allow_control = true;
        options.allow_direct = false;
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

    /// The size `device` shows now.
    fn shown(device: &str) -> Result<Mode> {
        let display = displays()?
            .into_iter()
            .find(|display| display.name == device)
            .ok_or("the display is gone")?;
        Ok(Mode {
            width: display.width,
            height: display.height,
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

    /// R13: does Windows put a `CDS_FULLSCREEN` change back when the process
    /// that made it exits? A child makes it and exits; this one looks.
    fn fullscreen_check(device: &str, fit: Mode, own: Mode) -> Result<()> {
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--fullscreen-child",
                &format!("{}x{}", fit.width, fit.height),
                "--device",
                device,
            ])
            .status()?;
        if !status.success() {
            return Err("the CDS_FULLSCREEN child failed".into());
        }
        std::thread::sleep(Duration::from_secs(2));
        let after = shown(device)?;
        let reverted = after == own;
        println!("CDS_FULLSCREEN reverts when its process exits: {reverted}");
        if !reverted {
            change(device, None, CDS_TYPE(0))?;
        }
        Ok(())
    }

    fn fullscreen_child(device: &str, fit: Mode) -> Result<()> {
        change(device, Some(fit), CDS_FULLSCREEN)?;
        std::thread::sleep(Duration::from_secs(2));
        Ok(())
    }
}

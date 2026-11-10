//! The cases, the host and viewer they drive, and the put-back.
use crate::modes::{Shown, change, every_display, print, read, shown};
use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{
    Host, HostOptions, Invitation, Mode, ModeList, ModeTarget, Pull, VideoCodec, Viewer,
    ViewerConfig, ViewerStatus, recover_display_modes,
};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    signal::windows::CtrlC,
};
use windows::Win32::Graphics::Gdi::{CDS_FULLSCREEN, CDS_TYPE, ENUM_REGISTRY_SETTINGS};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A host refuses another change this long after one.
const SETTLE: Duration = Duration::from_millis(3500);
/// The journal's file in the state directory (`docs/remote-desktop.md`).
const JOURNAL: &str = "display-modes.json";
/// How long the end waits for the ordinary desktop to put a display back.
const PUT_BACK_FOR: Duration = Duration::from_secs(600);

#[derive(Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
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
                (saved.width, saved.height, saved.hz) != (display.width, display.height, display.hz)
            }) {
                change(&display.device, None, CDS_TYPE(0))?;
            }
        }
        return print("now", &every_display()?);
    }
    if let Some(size) = option("--fullscreen-child") {
        let device = option("--device").ok_or("device required")?;
        change(&device, Some((parse(&size)?, 0)), CDS_FULLSCREEN)?;
        std::thread::sleep(Duration::from_secs(2));
        return Ok(());
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
    let fit = option("--fit").map(|size| parse(&size)).transpose()?;

    // An interrupted run's journal first, so the modes recorded are the
    // displays' own.
    let replayed = recover_display_modes(Some(&state_dir))?;
    if replayed > 0 {
        println!("an earlier run's journal put {replayed} display(s) back");
    }
    if state_dir.join(JOURNAL).exists() {
        return Err(format!(
            "an earlier run left a display mode that cannot be put back now; \
             run `display_mode_probe --restore --state-dir {}` once the \
             ordinary desktop shows",
            state_dir.display()
        )
        .into());
    }
    // One Ctrl-C receiver for the whole run: while none is alive, the
    // console's default handler ends the process at once, mid-restore.
    let mut interrupt = tokio::signal::windows::ctrl_c()?;
    let before = every_display()?;
    print("before", &before)?;
    let probe = Probe {
        client: &client,
        config_path,
        state_dir: &state_dir,
        before: &before,
    };
    let outcome = tokio::select! {
        outcome = probe.cases(fit, flag("--owner-change")) => outcome,
        _ = interrupt.recv() => {
            eprintln!(
                "interrupted: putting every display back (Ctrl-C again stops waiting \
                 for the ordinary desktop)"
            );
            Err("interrupted".into())
        }
    };
    let outcome = match outcome {
        Ok(fit) if flag("--fullscreen-check") => probe.fullscreen_check(fit),
        other => other.map(drop),
    };
    // Whatever happened, every display ends at its recorded mode; the
    // cases' hosts and child are gone by now.
    let put_back = probe.put_back(&mut interrupt).await;
    match every_display() {
        Ok(after) => print("after", &after)?,
        Err(failure) => println!("after: the displays cannot be read now ({failure})"),
    }
    println!(
        "input messages sent: 0 (the probe never calls Viewer::input or \
         Viewer::release_input)"
    );
    if let Err(failure) = &put_back {
        eprintln!(
            "NOT PUT BACK: {failure}. Run `display_mode_probe --restore --state-dir {}` \
             once the ordinary desktop shows.",
            state_dir.display()
        );
    }
    outcome?;
    put_back?;
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
    /// The cases; the size Fit local became.
    async fn cases(&self, requested: Option<Mode>, owner_change: bool) -> Result<Mode> {
        let host = start_host(self.client, self.state_dir).await?;
        let viewer = Controlling::connect(self.client, host.invitation()).await?;
        let device = viewer.device()?;
        let own = self.recorded(&device)?;
        let local = fit_for(requested, &viewer.modes()?)?;

        // 1: Fit local, then Original.
        let fit = viewer.change(ModeTarget::FitLocal(local)).await?;
        if fit == own.size() {
            return Err(
                format!("Fit local {}x{} changed nothing", local.width, local.height).into(),
            );
        }
        println!(
            "   Fit local {}x{} is {}x{}",
            local.width, local.height, fit.width, fit.height
        );
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
        // the journal. The child is killed on every way out of here.
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([self.config_path, "--child-host", "--state-dir"])
            .arg(self.state_dir)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut line = String::new();
        let output = child.stdout.take().ok_or("no child output")?;
        tokio::time::timeout(
            Duration::from_secs(30),
            BufReader::new(output).read_line(&mut line),
        )
        .await
        .map_err(|_| "the child host did not start")??;
        let invitation = Invitation::decode(line.trim())?;
        let killed = Controlling::connect(self.client, &invitation).await?;
        killed.change(ModeTarget::Size(fit)).await?;
        child.kill().await?;
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
                .modes()?
                .supported
                .into_iter()
                .find(|mode| *mode != fit && *mode != own.size())
                .ok_or("no third mode for the owner to pick")?;
            change(&device, Some((owners, 0)), CDS_TYPE(0))?;
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
        Ok(fit)
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

    /// Every display back at its recorded mode, trying every 2 s while
    /// it cannot be (the lock screen or a UAC prompt shows), for up to
    /// `PUT_BACK_FOR`. A Ctrl-C stops the wait; one pressed during an
    /// attempt is kept, and stops it once that attempt has finished.
    async fn put_back(&self, interrupt: &mut CtrlC) -> Result<()> {
        let until = Instant::now() + PUT_BACK_FOR;
        let mut said = None;
        loop {
            let failure = match self.try_put_back() {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            let text = failure.to_string();
            if said.as_ref() != Some(&text) {
                eprintln!(
                    "not back yet ({text}); waiting for the ordinary desktop, up to ten \
                     minutes. If it does not come, run `display_mode_probe --restore \
                     --state-dir {}`.",
                    self.state_dir.display()
                );
                said = Some(text);
            }
            if Instant::now() >= until {
                return Err(failure);
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(2)) => {}
                _ = interrupt.recv() => {
                    return Err("stopped waiting to put the displays back".into());
                }
            }
        }
    }

    /// The journal replayed; then a display not at its recorded mode is
    /// put back to its registry mode, which no case writes, and, where
    /// that differs, to the recorded mode itself.
    fn try_put_back(&self) -> Result<()> {
        let replayed = recover_display_modes(Some(self.state_dir))?;
        if replayed > 0 {
            println!("journal entries replayed at the end: {replayed}");
        }
        for display in every_display()? {
            let Some(own) = self.before.iter().find(|own| own.device == display.device) else {
                continue;
            };
            if display == *own {
                continue;
            }
            println!("putting {} back", display.device);
            change(&display.device, None, CDS_TYPE(0))?;
            if shown(&display.device)? != *own {
                change(&display.device, Some((own.size(), own.hz)), CDS_TYPE(0))?;
            }
        }
        if every_display()? != self.before {
            return Err("a display is not back at its recorded mode".into());
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
        let status = std::process::Command::new(std::env::current_exe()?)
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

/// The size Fit local is asked for: `requested`, else 1920x1080, which
/// must fit to another mode than the display shows. Without `--fit`, a
/// display that 1920x1080 would leave as it is gets the largest smaller
/// mode it offers, so a run always changes something.
fn fit_for(requested: Option<Mode>, modes: &ModeList) -> Result<Mode> {
    let local = requested.unwrap_or(Mode {
        width: 1920,
        height: 1080,
    });
    if modes
        .fit_local(local)
        .is_some_and(|fit| fit != modes.current)
    {
        return Ok(local);
    }
    if let Some(requested) = requested {
        return Err(format!(
            "--fit {}x{} would leave the display at {}x{}; pick another size it offers",
            requested.width, requested.height, modes.current.width, modes.current.height
        )
        .into());
    }
    let area = |mode: &Mode| u64::from(mode.width) * u64::from(mode.height);
    modes
        .supported
        .iter()
        .filter(|mode| area(mode) < area(&modes.current))
        .max_by_key(|mode| area(mode))
        .copied()
        .ok_or_else(|| "the display offers no smaller mode to change to".into())
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

/// A viewer that presents every unit at once and holds control. It has
/// no input call: the probe sends none.
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
            let mut capacity = 8 << 20;
            loop {
                match consumer.next(Duration::from_millis(50), capacity) {
                    Pull::Unit(unit) => {
                        consumer.presented(unit.generation, unit.frame_id);
                    }
                    Pull::NeedCapacity(needed) => capacity = needed,
                    Pull::Timeout | Pull::Paused => {}
                    Pull::Closed => return,
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
    fn modes(&self) -> Result<ModeList> {
        Ok(self.viewer.status().modes.ok_or("no display modes")?)
    }
    fn device(&self) -> Result<String> {
        Ok(self
            .viewer
            .status()
            .display
            .ok_or("no display reported")?
            .name)
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

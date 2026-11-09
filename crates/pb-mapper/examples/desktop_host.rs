//! Share this computer's display, until Ctrl+C.
//!
//! ```text
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --features desktop --example desktop_host -- \
//!   [--service desktop:NAME] [--display ID] [--allow-control] [--relay-only] \
//!   [--privileged] [--identity DIR]
//! ```
//!
//! It prints a `pbd1.` invitation for `desktop_viewer`. Viewers only watch
//! unless `--allow-control`. `--privileged` captures through the installed
//! helper, which also reaches UAC and the lock screen. `--identity DIR`
//! remembers the host's identity there, so the same invitation works after a
//! restart: the file holds a private key and a bearer grant, sealed for this
//! Windows user; deleting it revokes the invitation. Hosting needs Windows
//! today; elsewhere this prints why not.

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use pb_mapper::desktop::trust::{RelayScope, Remembered, TrustFile};
use pb_mapper::desktop::{self, Access, DisplaySelection, Host, HostOptions};
use pb_mapper::{Client, ClientConfig};

type Outcome<T = ()> = Result<T, Box<dyn Error>>;

#[tokio::main]
async fn main() -> Outcome {
    let support = desktop::host_support();
    if !support.host {
        println!("this computer cannot host: {:?}", support.reason);
        return Ok(());
    }
    let server = std::env::var("PB_MAPPER_SERVER")?;
    let mut options = HostOptions::new("desktop:example");
    let mut display = None;
    let mut identity = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--service" => options.service = value()?,
            "--display" => display = Some(DisplaySelection::Id(value()?.parse()?)),
            "--allow-control" => options.allow_control = true,
            "--relay-only" => options.allow_direct = false,
            "--privileged" => options.access = Access::Privileged,
            "--identity" => identity = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    let displays = desktop::displays()?;
    for display in &displays {
        println!(
            "display {}: {} {}x{} at {},{}",
            display.id, display.name, display.width, display.height, display.x, display.y
        );
    }
    let client = Client::new(ClientConfig {
        server: server.clone(),
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?;

    // A remembered grant resumes only on the relay it was made for, under the
    // same service; its display is found by device name, which outlives ids.
    let file = identity.map(|directory| TrustFile::<_, String>::new(directory, sealer()));
    let scope = RelayScope {
        relay: server,
        namespace: client.namespace().unwrap_or_default(),
    };
    let remembered = match &file {
        Some(file) => file
            .load()?
            .filter(|saved| saved.matches(&scope) && saved.settings == options.service),
        None => None,
    };
    options.display = display
        .or_else(|| {
            let device = remembered.as_ref()?.display_device.clone()?;
            Some(DisplaySelection::Device(device))
        })
        .unwrap_or_default();
    let display_device = match &options.display {
        DisplaySelection::Id(id) => displays
            .iter()
            .find(|display| display.id == *id)
            .map(|display| display.name.clone()),
        DisplaySelection::Device(device) => Some(device.clone()),
        DisplaySelection::Primary => None,
    };
    let mut builder = Host::builder(options);
    if let Some(saved) = remembered {
        println!("resuming the remembered identity");
        builder = builder.identity(saved.identity);
    }
    let host = builder.start(&client).await?;
    if let Some(file) = &file {
        file.save(&Remembered {
            scope,
            settings: host.options().service.clone(),
            identity: host.identity().clone(),
            display_device,
        })?;
        println!("remembered the identity; delete the file to revoke the invitation");
    }

    println!("Sharing. Anyone holding this invitation can view; keep it secret:");
    println!("{}", host.invitation().encode());
    let mut status = host.watch(Duration::from_millis(500));
    status.mark_changed();
    loop {
        tokio::select! {
            changed = status.changed() => {
                if changed.is_err() {
                    break;
                }
                let status = status.borrow_and_update().clone();
                match status.viewer {
                    Some(viewer) => println!(
                        "{:?}: a viewer on the {:?} path, controlling {}, paused {}, {:?}",
                        status.state, viewer.path, viewer.controlling, viewer.paused, viewer.quality
                    ),
                    None => println!("{:?}", status.state),
                }
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                host.stop().await?;
                break;
            }
        }
    }
    Ok(())
}

/// What seals the remembered identity: current-user DPAPI on Windows.
#[cfg(windows)]
fn sealer() -> desktop::trust::CurrentUser {
    desktop::trust::CurrentUser::TRUST
}

/// Elsewhere an integrator seals with the platform's keychain; this example
/// brings none, so it refuses to keep a secret in the clear.
#[cfg(not(windows))]
fn sealer() -> NoKeychain {
    NoKeychain
}
#[cfg(not(windows))]
struct NoKeychain;
#[cfg(not(windows))]
impl desktop::trust::Sealer for NoKeychain {
    fn seal(&self, _: &[u8]) -> desktop::Result<Vec<u8>> {
        Err(std::io::Error::other(
            "no keychain: implement trust::Sealer for this platform",
        ))
    }
    fn open(&self, plain: &[u8]) -> desktop::Result<Vec<u8>> {
        self.seal(plain)
    }
}

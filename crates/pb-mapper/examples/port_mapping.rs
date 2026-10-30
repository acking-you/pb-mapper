//! When a tunnel asks the router to map its direct port, and what follows.
//!
//! ```text
//! # On the computer that shares a service:
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example port_mapping -- share web 127.0.0.1:8080
//! # On another network, the computer that reaches it:
//! PB_MAPPER_SERVER=relay.example.com:7666 MSG_HEADER_KEY=<credential> \
//!   cargo run -p pb-mapper --example port_mapping -- connect web 127.0.0.1:9090 [auto|always|off]
//! ```
//!
//! A direct path needs each NAT to let the other end's first packet in. Many
//! do once this end has sent one out, and then hole punching is all it takes.
//! Some drop every packet they have not seen a reply to, and every connection
//! falls back to the relay. A router that speaks PCP, NAT-PMP or UPnP can be
//! asked for a mapping of its own: a public port that forwards to the
//! tunnel's UDP endpoint for as long as a lease runs, which the tunnel then
//! offers its peer first.
//!
//! `PortMapping::Auto`, the default, asks only once a direct attempt has
//! failed to punch through, so a network where punching works never hears
//! from it: the mapping waits on standby. The connection that failed uses the
//! relay; the next ones, once the lease is granted, offer the mapped address.
//! `Always` asks as soon as the first direct session opens the endpoint;
//! `Off` never asks. No session ever waits for the router.
//!
//! This prints each step: the punch that failed, the router found, the
//! request, the lease and its renewals. Connect to the local address on the
//! `connect` side to start a session. Press Ctrl-C to stop: the lease is
//! released before the tunnel reports that it has stopped.

use std::future::Future;

use pb_mapper::{
    Client, ClientConfig, ConnectOptions, ConnectRequest, DirectStage, PortMapping,
    PortMappingStatus, RegisterOptions, RegisterRequest, Transport, TunnelDiagnostics,
    TunnelEventKind, TunnelEvents,
};
use tokio::time::Instant;

const USAGE: &str = "usage: port_mapping share|connect <key> <local-addr> [auto|always|off]";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(role), Some(key), Some(local_addr)) = (args.next(), args.next(), args.next()) else {
        usage();
    };
    let share = match role.as_str() {
        "share" => true,
        "connect" => false,
        _ => usage(),
    };
    let port_mapping = match args.next().as_deref() {
        None | Some("auto") => PortMapping::Auto,
        Some("always") => PortMapping::Always,
        Some("off") => PortMapping::Off,
        Some(_) => usage(),
    };
    let client = Client::new(ClientConfig {
        server: std::env::var("PB_MAPPER_SERVER")?,
        credential: std::env::var("MSG_HEADER_KEY")?,
        keep_alive: true,
        namespace: None,
    })?;
    if share {
        let registration = client
            .register_with_options(
                RegisterRequest {
                    key,
                    local_addr,
                    transport: Transport::Tcp,
                    codec: false,
                    force_namespace: false,
                },
                RegisterOptions {
                    port_mapping,
                    ..Default::default()
                },
            )
            .await?;
        watch(
            registration.subscribe_events(),
            || registration.diagnostics(),
            registration.stop(),
        )
        .await
    } else {
        let connection = client
            .connect_with_options(
                ConnectRequest {
                    key,
                    local_addr,
                    transport: Transport::Tcp,
                },
                ConnectOptions {
                    port_mapping,
                    ..Default::default()
                },
            )
            .await?;
        watch(
            connection.subscribe_events(),
            || connection.diagnostics(),
            connection.stop(),
        )
        .await
    }
}

/// Print the tunnel's port mapping and direct outcomes until it has stopped,
/// stopping it on Ctrl-C.
async fn watch(
    mut events: TunnelEvents,
    diagnostics: impl Fn() -> TunnelDiagnostics,
    stop: impl Future<Output = pb_mapper::Result<()>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let stamp = || format!("{:7.1}s", started.elapsed().as_secs_f32());
    // On standby, or waiting for the first direct session to open its port.
    if let Some(mapping) = diagnostics().port_mapping {
        println!("{}  mapping  {}", stamp(), describe(&mapping.current));
    }
    let mut stop = std::pin::pin!(stop);
    let mut stopping = false;
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c(), if !stopping => {
                result?;
                stopping = true;
                println!("{}  stopping; the lease is released first", stamp());
                stop.as_mut().await?;
            }
            event = events.recv() => match event {
                Some(Ok(event)) => match event.kind {
                    TunnelEventKind::PortMappingChanged { mapping } => {
                        println!("{}  mapping  {}", stamp(), describe(&mapping));
                    }
                    TunnelEventKind::DirectEstablished { setup_ms } => {
                        println!("{}  direct   open, set up in {setup_ms} ms", stamp());
                    }
                    // The cue `Auto` waits for: the punch did not get through.
                    TunnelEventKind::DirectFallback { stage: DirectStage::Punching } => {
                        println!("{}  relay    the punch did not get through", stamp());
                    }
                    TunnelEventKind::DirectFallback { stage } => {
                        println!("{}  relay    direct gave way at {}", stamp(), stage.as_str());
                    }
                    TunnelEventKind::Stopped => println!("{}  stopped", stamp()),
                    _ => {}
                },
                // Fell behind: what the mapping is now is in the snapshot.
                Some(Err(lagged)) => {
                    eprintln!("{lagged}");
                    if let Some(mapping) = diagnostics().port_mapping {
                        println!("{}  mapping  {}", stamp(), describe(&mapping.current));
                    }
                }
                None => return Ok(()),
            },
        }
    }
}

/// "active via pcp, :51820 -> 203.0.113.7:51820 for 7200 s".
fn describe(mapping: &PortMappingStatus) -> String {
    let mut line = mapping.stage.as_str().to_string();
    if let Some(protocol) = mapping.protocol {
        line += &format!(" via {}", protocol.as_str());
    }
    if let Some(gateway) = mapping.gateway {
        line += &format!(", router {gateway}");
    }
    if let (true, Some(external)) = (mapping.active, mapping.external_addr) {
        let local = mapping
            .local_port
            .map(|port| format!(":{port} "))
            .unwrap_or_default();
        line += &format!(", {local}-> {external}");
        if let Some(lease) = mapping.lease_lifetime_ms {
            line += &format!(" for {} s", lease / 1000);
        }
    }
    if let Some(error) = &mapping.error {
        line += &format!(" ({error})");
    }
    line
}

fn usage() -> ! {
    eprintln!("{USAGE}");
    std::process::exit(2);
}

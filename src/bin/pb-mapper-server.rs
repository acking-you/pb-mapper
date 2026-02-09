use better_mimalloc_rs::MiMalloc;
use clap::Parser;
use pb_mapper::common::checksum::{setup_machine_msg_header_key, MACHINE_MSG_HEADER_KEY_PATH};
use pb_mapper::common::config::{init_tracing, PB_MAPPER_KEEP_ALIVE};
use pb_mapper::pb_server::run_server;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[global_allocator]
static GLOBAL_MIMALLOC: MiMalloc = MiMalloc;

#[derive(Parser)]
#[command(author = "L_B__", version, about, long_about = None)]
struct Cli {
    /// [optional] Port exposed for use by local services,default value is `7666`
    #[arg(short, long, default_value_t = 7666)]
    pb_mapper_port: u16,
    /// [optional] Used to enable ipv6 listening
    #[arg(long, default_value_t = false)]
    use_ipv6: bool,
    /// [optional] keep-alive for connection stream. by default, it is false.Note that keepalive is
    /// also controlled by the env:`PB_MAPPER_KEEP_ALIVE`.
    #[arg(
        short,
        long,
        value_name = "PB_MAPPER_KEEP_ALIVE",
        default_value_t = false
    )]
    keep_alive: bool,
    /// [optional] derive `MSG_HEADER_KEY` from machine hostname + MAC addresses and persist to
    /// `/var/lib/pb-mapper-server/msg_header_key` so operators can reuse the same key in
    /// `pb-mapper-server-cli` / `pb-mapper-client-cli`.
    #[arg(long, default_value_t = false)]
    use_machine_msg_header_key: bool,
}

#[tokio::main]
async fn main() {
    MiMalloc::init();
    let cli = Cli::parse();
    init_tracing();
    if cli.keep_alive {
        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
    }
    if cli.use_machine_msg_header_key {
        match setup_machine_msg_header_key() {
            Ok(_) => {
                tracing::info!(
                    "derived and persisted machine MSG_HEADER_KEY to: {}",
                    MACHINE_MSG_HEADER_KEY_PATH
                );
            }
            Err(err) => {
                tracing::error!(
                    "failed to derive machine MSG_HEADER_KEY and write to {}: {}",
                    MACHINE_MSG_HEADER_KEY_PATH,
                    err
                );
                std::process::exit(1);
            }
        }
    }
    let ip_addr = if cli.use_ipv6 {
        IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0))
    } else {
        IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))
    };
    if let Err(e) = run_server((ip_addr, cli.pb_mapper_port)).await {
        tracing::error!("Failed to start pb-mapper server: {e}");
    }
}

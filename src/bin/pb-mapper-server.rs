use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use clap::Parser;
use mimalloc_rust::GlobalMiMalloc;
use pb_mapper::common::config::{init_tracing, PB_MAPPER_KEEP_ALIVE};
use pb_mapper::pb_server::run_server;

#[global_allocator]
static GLOBAL_MIMALLOC: GlobalMiMalloc = GlobalMiMalloc;

#[derive(Parser)]
#[command(author = "L_B__", version, about, long_about = None)]
struct Cli {
    /// [optional] Port exposed for use by local services,default value is `7666`
    #[arg(short, long, default_value_t = 7666)]
    pb_mapper_port: u16,
    /// [optional] Used to enable ipv6 listening
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing();
    if cli.keep_alive {
        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
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

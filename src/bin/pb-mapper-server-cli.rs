use clap::Parser;
use mimalloc_rust::GlobalMiMalloc;
use pb_mapper::common::config::{get_pb_mapper_server, get_sockaddr, init_tracing, LocalService};
use pb_mapper::common::stream::{StreamProvider, TcpStreamProvider, UdpStreamProvider};
use pb_mapper::local::client::handle_status_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::snafu_error_get_or_return;

#[global_allocator]
static GLOBAL_MIMALLOC: GlobalMiMalloc = GlobalMiMalloc;

#[derive(Parser)]
#[command(author = "L_B__", version, about, long_about = None)]
struct Cli {
    /// Local service that need to be exposed
    #[command(subcommand)]
    local_server: LocalService,
    /// [optional] Remote service registry, note that you need to include IP and port,such as
    /// `127.0.0.1:1080`. by default, we take the value  from env:`PB_MAPPER_SERVER`
    #[arg(short, long, value_name = "PB_MAPPER_SERVER")]
    pb_mapper_server: Option<String>,
    /// [optional] keep-alive for local server stream. by default, it is false
    #[arg(short, long, default_value_t = false)]
    keep_alive: bool,
}

async fn run_with_keepalive<LocalStream: StreamProvider>(
    keepalive: bool,
    key: String,
    local_addr: &str,
    remote_addr: Option<&str>,
) {
    let local_addr = snafu_error_get_or_return!(get_sockaddr(local_addr));
    let remote_addr = snafu_error_get_or_return!(get_pb_mapper_server(remote_addr));
    if keepalive {
        run_server_side_cli::<true, LocalStream, _>(local_addr, remote_addr, key.into()).await
    } else {
        run_server_side_cli::<false, LocalStream, _>(local_addr, remote_addr, key.into()).await
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing();
    match cli.local_server {
        LocalService::UdpServer { key, addr } => {
            run_with_keepalive::<UdpStreamProvider>(
                cli.keep_alive,
                key,
                &addr,
                cli.pb_mapper_server.as_deref(),
            )
            .await
        }
        LocalService::TcpServer { key, addr } => {
            run_with_keepalive::<TcpStreamProvider>(
                cli.keep_alive,
                key,
                &addr,
                cli.pb_mapper_server.as_deref(),
            )
            .await
        }
        LocalService::Status { op } => {
            handle_status_cli(
                op,
                snafu_error_get_or_return!(get_pb_mapper_server(cli.pb_mapper_server.as_deref())),
            )
            .await
        }
    }
}

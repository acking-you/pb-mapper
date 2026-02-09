use better_mimalloc_rs::MiMalloc;
use clap::Parser;
use pb_mapper::common::config::{
    get_pb_mapper_server_async, get_sockaddr_async, init_tracing, LocalService,
    PB_MAPPER_KEEP_ALIVE,
};
use pb_mapper::common::message::forward::StreamForward;
use pb_mapper::local::client::handle_status_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::snafu_error_get_or_return;
use uni_stream::stream::{StreamProvider, TcpStreamProvider, UdpStreamProvider};

#[global_allocator]
static GLOBAL_MIMALLOC: MiMalloc = MiMalloc;
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
    /// [optional] keep-alive for local server stream. by default, it is false.Note that
    /// keep-alive is also controlled by the env:`PB_MAPPER_KEEP_ALIVE`.
    #[arg(
        short,
        long,
        value_name = "PB_MAPPER_KEEP_ALIVE",
        default_value_t = false
    )]
    keep_alive: bool,
    /// [optional] enable codec mode when forward message
    #[arg(short, long)]
    codec: bool,
}

async fn run_register<LocalStream: StreamProvider>(
    need_codec: bool,
    is_datagram: bool,
    key: String,
    local_addr: &str,
    remote_addr: Option<&str>,
) where
    LocalStream::Item: StreamForward,
{
    let local_addr = snafu_error_get_or_return!(get_sockaddr_async(local_addr).await);
    let remote_addr = snafu_error_get_or_return!(get_pb_mapper_server_async(remote_addr).await);
    run_server_side_cli::<LocalStream, _>(
        local_addr,
        remote_addr,
        key.into(),
        need_codec,
        is_datagram,
    )
    .await
}

#[tokio::main]
async fn main() {
    MiMalloc::init();
    let cli = Cli::parse();
    init_tracing();
    if cli.keep_alive {
        std::env::set_var(PB_MAPPER_KEEP_ALIVE, "ON");
    }
    match cli.local_server {
        LocalService::UdpServer { key, addr } => {
            run_register::<UdpStreamProvider>(
                cli.codec,
                true,
                key,
                &addr,
                cli.pb_mapper_server.as_deref(),
            )
            .await
        }
        LocalService::TcpServer { key, addr } => {
            run_register::<TcpStreamProvider>(
                cli.codec,
                false,
                key,
                &addr,
                cli.pb_mapper_server.as_deref(),
            )
            .await
        }
        LocalService::Status { op } => {
            handle_status_cli(
                op,
                snafu_error_get_or_return!(
                    get_pb_mapper_server_async(cli.pb_mapper_server.as_deref()).await
                ),
            )
            .await
        }
    }
}

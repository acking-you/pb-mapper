use super::*;

#[derive(Debug, Args)]
pub(super) struct AdminArgs {
    /// Relay address. Falls back to PB_MAPPER_SERVER.
    #[arg(short, long, visible_alias = "pb-mapper-server", value_name = "ADDR")]
    server: Option<String>,
    /// Machine-readable output mode.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    output: OutputFormat,
    #[command(subcommand)]
    command: AdminCommand,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Issue, inspect, renew, reveal, revoke, or collect temporary keys.
    Key(AdminKeyArgs),
    /// List relay connections across namespaces.
    Connection(AdminConnectionArgs),
    /// List registered services across namespaces.
    Service(AdminServiceArgs),
    /// Show authentication state and protocol counters.
    Status,
    /// Repair or reset encrypted temporary-key state.
    AuthState(AdminAuthStateArgs),
    /// Rotate the sole administrator key and invalidate every existing credential.
    RootKey(AdminRootKeyArgs),
    /// Change legacy protocol acceptance at runtime.
    LegacyProtocol(AdminLegacyProtocolArgs),
}

#[derive(Debug, Args)]
struct AdminKeyArgs {
    #[command(subcommand)]
    command: AdminKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminKeyCommand {
    Issue {
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
        #[arg(long)]
        label: Option<String>,
    },
    List {
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
    Show {
        key_id: u64,
    },
    Reveal {
        key_id: u64,
    },
    Renew {
        key_id: u64,
        #[arg(long, value_parser = parse_duration)]
        ttl: Duration,
    },
    Revoke {
        key_id: u64,
    },
    Gc,
}

#[derive(Debug, Args)]
struct AdminConnectionArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Args)]
struct AdminServiceArgs {
    #[command(subcommand)]
    command: AdminListCommand,
}

#[derive(Debug, Clone, Subcommand)]
enum AdminListCommand {
    List {
        #[arg(long)]
        key_id: Option<u64>,
        #[arg(long, default_value_t = 0)]
        page: u32,
        #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
        page_size: u16,
        #[arg(long, default_value_t = false)]
        all: bool,
    },
}

#[derive(Debug, Args)]
struct AdminAuthStateArgs {
    #[command(subcommand)]
    command: AdminAuthStateCommand,
}

#[derive(Debug, Subcommand)]
enum AdminAuthStateCommand {
    Reset {
        #[arg(long, default_value_t = false)]
        confirm: bool,
    },
}

#[derive(Debug, Args)]
struct AdminRootKeyArgs {
    #[command(subcommand)]
    command: AdminRootKeyCommand,
}

#[derive(Debug, Subcommand)]
enum AdminRootKeyCommand {
    Rotate {
        /// New 32-byte administrator key. A cryptographically random printable key is generated when omitted.
        #[arg(long)]
        new_key: Option<String>,
        /// Save the new key here before asking the relay to rotate.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct AdminLegacyProtocolArgs {
    #[command(subcommand)]
    command: AdminLegacyProtocolCommand,
}

#[derive(Debug, Subcommand)]
enum AdminLegacyProtocolCommand {
    Set { policy: LegacyProtocolArg },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
    Ndjson,
}

pub(super) async fn run_admin(args: AdminArgs) -> Result<(), Box<dyn Error>> {
    let remote_addr = get_pb_mapper_server_async(args.server.as_deref()).await?;
    match args.command {
        AdminCommand::Key(AdminKeyArgs { command }) => match command {
            AdminKeyCommand::Issue { ttl, label } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyIssue {
                        ttl_seconds: ttl.as_secs(),
                        label,
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::List {
                page,
                page_size,
                all,
            } => {
                stream_key_pages(remote_addr, args.output, page, page_size, all).await?;
            }
            AdminKeyCommand::Show { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyShow { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Reveal { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyReveal { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Renew { key_id, ttl } => {
                let response = send_admin_request(
                    remote_addr,
                    AdminRequest::KeyRenew {
                        key_id,
                        ttl_seconds: ttl.as_secs(),
                    },
                )
                .await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Revoke { key_id } => {
                let response =
                    send_admin_request(remote_addr, AdminRequest::KeyRevoke { key_id }).await?;
                print_admin_response(args.output, &response)?;
            }
            AdminKeyCommand::Gc => {
                let response = send_admin_request(remote_addr, AdminRequest::KeyGc).await?;
                print_admin_response(args.output, &response)?;
            }
        },
        AdminCommand::Connection(AdminConnectionArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_connection_pages(remote_addr, args.output, key_id, page, page_size, all).await?;
        }
        AdminCommand::Service(AdminServiceArgs { command }) => {
            let AdminListCommand::List {
                key_id,
                page,
                page_size,
                all,
            } = command;
            stream_service_pages(remote_addr, args.output, key_id, page, page_size, all).await?;
        }
        AdminCommand::Status => {
            let response = send_admin_request(remote_addr, AdminRequest::AuthStatus).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::AuthState(AdminAuthStateArgs {
            command: AdminAuthStateCommand::Reset { confirm },
        }) => {
            let response =
                send_admin_request(remote_addr, AdminRequest::AuthStateReset { confirm }).await?;
            print_admin_response(args.output, &response)?;
        }
        AdminCommand::RootKey(AdminRootKeyArgs {
            command: AdminRootKeyCommand::Rotate { new_key, key_file },
        }) => {
            let key_file = key_file.unwrap_or_else(default_admin_recovery_key_file);
            let new_key = new_key.unwrap_or_else(generate_admin_key);
            let staged_key_file = key_file.with_file_name(format!(
                ".{}.next",
                key_file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("admin.key")
            ));
            write_admin_key_file(&staged_key_file, &new_key, true)?;
            let response = send_admin_request(
                remote_addr,
                AdminRequest::RootKeyRotate {
                    new_admin_key: new_key.clone(),
                },
            )
            .await
            .map_err(|error| {
                std::io::Error::other(format!(
                    "root rotation request failed; the candidate key remains at `{}`: {error}",
                    staged_key_file.display()
                ))
            })?;
            set_process_msg_header_key(Some(&new_key))?;
            let verification = send_admin_request(remote_addr, AdminRequest::AuthStatus).await?;
            if !matches!(verification, AdminResponse::AuthStatus(_)) {
                return Err(std::io::Error::other(
                    "new administrator key did not pass the post-rotation status check",
                )
                .into());
            }
            write_admin_key_file(&key_file, &new_key, true).map_err(|error| {
                std::io::Error::other(format!(
                    "administrator key rotated and verified, but `{}` could not be updated; recover the key from `{}`: {error}",
                    key_file.display(),
                    staged_key_file.display()
                ))
            })?;
            if let Err(error) = std::fs::remove_file(&staged_key_file) {
                tracing::warn!(
                    path = %staged_key_file.display(),
                    %error,
                    "administrator key was rotated, but the staged key file could not be removed"
                );
            }
            if args.output == OutputFormat::Human {
                println!("administrator key rotated and verified");
                println!("key file: {}", key_file.display());
            } else {
                print_admin_response(args.output, &response)?;
            }
        }
        AdminCommand::LegacyProtocol(AdminLegacyProtocolArgs {
            command: AdminLegacyProtocolCommand::Set { policy },
        }) => {
            let response = send_admin_request(
                remote_addr,
                AdminRequest::LegacyProtocolSet {
                    policy: policy.into(),
                },
            )
            .await?;
            print_admin_response(args.output, &response)?;
        }
    }
    Ok(())
}

async fn send_admin_request(
    remote_addr: std::net::SocketAddr,
    request: AdminRequest,
) -> Result<AdminResponse, Box<dyn Error>> {
    let encoded = PbConnRequest::Admin(request).encode()?;
    for attempt in 0..2 {
        let mut stream = TcpStream::connect(remote_addr).await?;
        let session = ClientHeaderSession::from_process()?;
        session.write_initial(&mut stream, &encoded).await?;
        let mut reader = session.response_reader(&mut stream)?;
        let message = reader.read_msg().await?;
        match PbConnResponse::decode(message)? {
            PbConnResponse::Admin(response) => return Ok(response),
            PbConnResponse::Error(error)
                if error.code == "connection_salt_replayed" && error.retryable && attempt == 0 =>
            {
                continue;
            }
            PbConnResponse::Error(error) => {
                return Err(std::io::Error::other(format!(
                    "{}: {} (retryable={})",
                    error.code, error.message, error.retryable
                ))
                .into());
            }
            response => {
                return Err(std::io::Error::other(format!(
                    "unexpected administrator response: {response:?}"
                ))
                .into());
            }
        }
    }
    Err(std::io::Error::other("connection salt replay retry was exhausted").into())
}

async fn stream_key_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    let mut combined: Option<KeyPage> = None;
    loop {
        let response =
            send_admin_request(remote_addr, AdminRequest::KeyList { page, page_size }).await?;
        let AdminResponse::KeyList(key_page) = &response else {
            return Err(std::io::Error::other("unexpected key-list response").into());
        };
        if all {
            if output == OutputFormat::Ndjson {
                for item in &key_page.items {
                    println!("{}", serde_json::to_string(item)?);
                }
            } else {
                let page = combined.get_or_insert_with(|| {
                    let mut page = key_page.clone();
                    page.items.clear();
                    page.next_page = None;
                    page
                });
                page.items.extend(key_page.items.iter().cloned());
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = key_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    if let Some(page) = combined {
        print_admin_response(output, &AdminResponse::KeyList(page))?;
    }
    Ok(())
}

async fn stream_service_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    key_id: Option<u64>,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    let mut combined: Option<AdminServicePage> = None;
    loop {
        let response = send_admin_request(
            remote_addr,
            AdminRequest::ServiceList {
                key_id,
                page,
                page_size,
            },
        )
        .await?;
        let AdminResponse::Services(service_page) = &response else {
            return Err(std::io::Error::other("unexpected service-list response").into());
        };
        if all {
            if output == OutputFormat::Ndjson {
                for item in &service_page.items {
                    println!("{}", serde_json::to_string(item)?);
                }
            } else {
                let page = combined.get_or_insert_with(|| {
                    let mut page = service_page.clone();
                    page.items.clear();
                    page.next_page = None;
                    page
                });
                page.items.extend(service_page.items.iter().cloned());
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = service_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    if let Some(page) = combined {
        print_admin_response(output, &AdminResponse::Services(page))?;
    }
    Ok(())
}

async fn stream_connection_pages(
    remote_addr: std::net::SocketAddr,
    output: OutputFormat,
    key_id: Option<u64>,
    mut page: u32,
    page_size: u16,
    all: bool,
) -> Result<(), Box<dyn Error>> {
    let mut combined: Option<AdminConnectionPage> = None;
    loop {
        let response = send_admin_request(
            remote_addr,
            AdminRequest::ConnectionList {
                key_id,
                page,
                page_size,
            },
        )
        .await?;
        let AdminResponse::Connections(connection_page) = &response else {
            return Err(std::io::Error::other("unexpected connection-list response").into());
        };
        if all {
            if output == OutputFormat::Ndjson {
                for item in &connection_page.items {
                    println!("{}", serde_json::to_string(item)?);
                }
            } else {
                let page = combined.get_or_insert_with(|| {
                    let mut page = connection_page.clone();
                    page.items.clear();
                    page.next_page = None;
                    page
                });
                page.items.extend(connection_page.items.iter().cloned());
            }
        } else {
            print_admin_response(output, &response)?;
        }
        let Some(next_page) = connection_page.next_page else {
            break;
        };
        if !all {
            break;
        }
        page = next_page;
    }
    if let Some(page) = combined {
        print_admin_response(output, &AdminResponse::Connections(page))?;
    }
    Ok(())
}

fn print_admin_response(
    output: OutputFormat,
    response: &AdminResponse,
) -> Result<(), Box<dyn Error>> {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "data": response,
            }))?
        ),
        OutputFormat::Ndjson => println!("{}", serde_json::to_string(response)?),
        OutputFormat::Human => print_human_admin_response(response),
    }
    Ok(())
}

fn print_human_admin_response(response: &AdminResponse) {
    match response {
        AdminResponse::KeyIssued(key)
        | AdminResponse::KeyShown(key)
        | AdminResponse::KeyRenewed(key) => {
            println!("key id: {}", key.metadata.key_id);
            println!("state: {}", key.metadata.state);
            println!("expires at: {}", key.metadata.expires_at);
            if let Some(label) = &key.metadata.label {
                println!("label: {label}");
            }
            if !key.credential.is_empty() {
                println!("credential: {}", key.credential);
            }
        }
        AdminResponse::KeyRevoked(key) => {
            println!("key {}: {}", key.key_id, key.state);
        }
        AdminResponse::KeyList(page) => {
            println!("KEY ID\tSTATE\tEXPIRES\tLABEL");
            for key in &page.items {
                println!(
                    "{}\t{}\t{}\t{}",
                    key.key_id,
                    key.state,
                    key.expires_at,
                    key.label.as_deref().unwrap_or("")
                );
            }
            if let Some(next) = page.next_page {
                println!("next page: {next}");
            }
        }
        AdminResponse::KeyGc { removed } => println!("removed {removed} inactive keys"),
        AdminResponse::AuthStatus(status) => {
            println!("safe mode: {}", status.safe_mode);
            println!(
                "keys: {} active / {} expired / {} revoked / {} capacity",
                status.active_keys, status.expired_keys, status.revoked_keys, status.capacity
            );
            println!("legacy protocol: {:?}", status.legacy_protocol);
            println!(
                "active legacy connections: {}",
                status.active_legacy_connections
            );
            println!(
                "last legacy connection: {}",
                status
                    .last_legacy_connection_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".to_string())
            );
            println!(
                "authentication: {} succeeded / {} failed",
                status.auth_successes, status.auth_failures
            );
            println!("server instance: {}", status.server_instance_id);
        }
        AdminResponse::Services(page) => {
            println!("KEY ID\tSERVICE\tTRANSPORT\tCODEC\tCONNECTIONS");
            for service in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    service.key_id,
                    service.service_name,
                    service.transport,
                    service.codec_enabled,
                    service.connection_count
                );
            }
        }
        AdminResponse::Connections(page) => {
            println!("KEY ID\tSERVICE\tCONN ID\tHEALTHY\tTRANSPORT\tCODEC");
            for connection in &page.items {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    connection.key_id,
                    connection.service_name,
                    connection.conn_id,
                    connection.healthy,
                    connection.transport,
                    connection.codec_enabled
                );
            }
        }
        AdminResponse::Ok { action } => println!("ok: {action}"),
    }
}

fn default_admin_recovery_key_file() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pb-mapper")
        .join("admin.key")
}

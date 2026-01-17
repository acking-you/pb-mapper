use std::env;
use std::sync::LazyLock;
use std::time::Duration;

use pb_mapper::common::config::init_tracing;
use pb_mapper::common::message::{
    MessageReader, MessageWriter, NormalMessageReader, NormalMessageWriter,
};
use pb_mapper::local::client::run_client_side_cli;
use pb_mapper::local::server::run_server_side_cli;
use pb_mapper::pb_server::run_server;
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::time::{timeout, Instant};
use uni_stream::addr::ToSocketAddrs;
use uni_stream::stream::{ListenerProvider, TcpListenerProvider, UdpListenerProvider};
use uni_stream::stream::{StreamProvider, StreamSplit, TcpStreamProvider, UdpStreamProvider};
use uni_stream::udp::tune_udp_socket;

struct TimerTickGurad<'a> {
    ins: Instant,
    mut_duration: &'a mut Duration,
}

impl<'a> TimerTickGurad<'a> {
    fn new(mut_duration: &'a mut Duration) -> Self {
        Self {
            ins: Instant::now(),
            mut_duration,
        }
    }
}

impl<'a> Drop for TimerTickGurad<'a> {
    fn drop(&mut self) {
        let end = Instant::now();
        let duration = end - self.ins;
        *self.mut_duration += duration;
        println!("duration:{duration:?}");
    }
}

use uni_stream::stream::StreamAccept;

const UDP_TEST_PAYLOAD_MAX: usize = 1200;

async fn echo_server<P: ListenerProvider>(
    server_addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = P::bind(server_addr).await?;
    println!("run echo server:{server_addr}");
    loop {
        // Accept incoming connections
        let (mut stream, addr) = listener.accept().await?;
        println!("Connected from {addr}");

        // Process each connection concurrently
        tokio::spawn(async move {
            // Read data from client
            let mut buf = vec![0; 1024];
            loop {
                let n = match stream.read(&mut buf).await {
                    Ok(n) => n,
                    Err(e) => {
                        println!("Error reading: {e}");
                        return;
                    }
                };

                // If no data received, assume disconnect
                if n == 0 {
                    return;
                }

                // Echo data back to client
                if let Err(e) = stream.write_all(&buf[..n]).await {
                    println!("Error writing: {e}");
                    return;
                }

                println!("Echoed {n} bytes to {addr}");
            }
        });
    }
}

async fn run_echo_server(
    server_type: ServerType,
    addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    match server_type {
        ServerType::Udp => run_udp_echo_server(addr).await,
        ServerType::Tcp => echo_server::<TcpListenerProvider>(addr).await,
    }
}

async fn run_udp_echo_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind(addr).await?;
    tune_udp_socket(&socket);
    let mut buf = vec![0u8; 65_507];
    println!("run udp echo server:{addr}");
    loop {
        let (len, peer) = socket.recv_from(&mut buf).await?;
        if let Err(err) = socket.send_to(&buf[..len], peer).await {
            println!("udp echo send error: {err}");
        }
    }
}

async fn run_pb_mapper_server(addr: &str) {
    run_server(addr).await;
}

async fn run_pb_mapper_server_cli(
    server_type: ServerType,
    local_addr: &str,
    remote_addr: &str,
    key: &str,
    need_codec: bool,
) {
    match server_type {
        ServerType::Udp => {
            run_server_side_cli::<UdpStreamProvider, _>(
                local_addr,
                remote_addr,
                key.into(),
                need_codec,
                true,
            )
            .await
        }
        ServerType::Tcp => {
            run_server_side_cli::<TcpStreamProvider, _>(
                local_addr,
                remote_addr,
                key.into(),
                need_codec,
                false,
            )
            .await
        }
    }
}

async fn run_pb_mapper_client_cli(
    server_type: ServerType,
    local_addr: &str,
    remote_addr: &str,
    key: &str,
) {
    match server_type {
        ServerType::Udp => {
            run_client_side_cli::<UdpListenerProvider, _>(
                local_addr.to_string(),
                remote_addr.to_string(),
                key.into(),
            )
            .await
        }
        ServerType::Tcp => {
            run_client_side_cli::<TcpListenerProvider, _>(
                local_addr.to_string(),
                remote_addr.to_string(),
                key.into(),
            )
            .await
        }
    }
}

/// get random message
fn gen_random_msg(max_len: usize) -> Vec<u8> {
    let len = rand::rng().random_range(0_usize..max_len);
    let mut vec = Vec::new();
    for _ in 0..len {
        vec.push(rand::rng().random_range(0..212));
    }
    vec
}

async fn run_udp_datagram_echo(addr: &str, rounds: usize, burst: usize) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    tune_udp_socket(&socket);
    socket.connect(addr).await.unwrap();
    let mut buf = vec![0u8; 65_507];

    // Warm up UDP path to ensure listener and forwarding pipeline are ready.
    let probe = b"pb-mapper-probe";
    let mut ready = false;
    for _ in 0..10 {
        socket.send(probe).await.unwrap();
        if let Ok(Ok(len)) = timeout(Duration::from_millis(300), socket.recv(&mut buf)).await {
            if &buf[..len] == probe {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ready, "udp echo path not ready");

    for round in 0..rounds {
        for seq in 0..burst {
            let mut msg = (seq as u32).to_be_bytes().to_vec();
            if round == 0 && seq == 0 {
                msg.extend(vec![0u8; UDP_TEST_PAYLOAD_MAX]);
            } else {
                msg.extend(gen_random_msg(UDP_TEST_PAYLOAD_MAX));
            }
            socket.send(&msg).await.unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let wait = deadline.saturating_duration_since(Instant::now());
                let len = match timeout(wait, socket.recv(&mut buf)).await {
                    Ok(Ok(len)) => len,
                    Ok(Err(err)) => panic!("udp recv error: {err}"),
                    Err(_) => panic!("udp echo timeout; missing seq: {seq}"),
                };
                if len < 4 {
                    continue;
                }
                let recv_seq = u32::from_be_bytes(buf[..4].try_into().unwrap());
                if recv_seq != seq as u32 {
                    continue;
                }
                assert_eq!(msg, &buf[..len]);
                break;
            }
        }
    }
}

async fn run_echo_delay<P: StreamProvider, A: ToSocketAddrs + Send>(addr: A, times: usize) {
    let mut stream = P::from_addr(addr).await.unwrap();
    let (mut reader, mut writer) = stream.split();
    let mut reader = NormalMessageReader::new(&mut reader);
    let mut writer = NormalMessageWriter::new(&mut writer);
    let mut duration = Duration::default();
    for _ in 0..times {
        let expected = gen_random_msg(2000);
        for _ in 0..10 {
            let msg = {
                let _guard = TimerTickGurad::new(&mut duration);
                writer.write_msg(&expected).await.unwrap();
                reader.read_msg().await.unwrap()
            };

            assert_eq!(expected, msg);
        }
    }
    println!(
        "{} rounds of 10 random data echo delay tests each took a total of {:?}",
        times, duration
    );
}

#[derive(Debug, Clone, Copy)]
enum ServerType {
    Udp,
    Tcp,
}

static PB_MAPPER_SERVER: LazyLock<String> =
    LazyLock::new(|| env::var("PB_MAPPER_TEST_SERVER").unwrap());

static LOCAL_SERVER: LazyLock<String> = LazyLock::new(|| env::var("LOCAL_TEST_SERVER").unwrap());

static ECHO_SERVER: LazyLock<String> = LazyLock::new(|| env::var("ECHO_TEST_SERVER").unwrap());

static SERVER_KEY: LazyLock<String> = LazyLock::new(|| env::var("SERVER_TEST_KEY").unwrap());

static SERVER_TYPE: LazyLock<ServerType> = LazyLock::new(|| {
    if env::var("SERVER_TEST_TYPE").unwrap() == "UDP" {
        ServerType::Udp
    } else {
        ServerType::Tcp
    }
});

static INIT_TRACING: LazyLock<()> = LazyLock::new(|| {
    println!("{:?}", env::current_dir().unwrap());
    dotenvy::from_filename(env::current_dir().unwrap().join("tests").join(".env")).unwrap();
    init_tracing();
});

/// This is only for testing the correctness of the logic, for performance testing of latency,
/// please run a separate binary.
#[ignore = "run codec test enough"]
#[tokio::test]
async fn test_pb_mapper_server_no_codec() {
    *INIT_TRACING;
    // run echo server
    let remote_echo = ECHO_SERVER.clone();
    let server_type = *SERVER_TYPE;
    let pb_mapper_server = PB_MAPPER_SERVER.clone();
    let server_key = SERVER_KEY.clone();
    let echo_server = ECHO_SERVER.clone();
    let local_server = LOCAL_SERVER.clone();

    let echo_server_handle =
        tokio::spawn(async move { run_echo_server(server_type, &remote_echo).await.unwrap() });
    // run pb-mapper-server
    let pb_server = pb_mapper_server.clone();
    let pb_mapper_server_handle = tokio::spawn(async move {
        run_pb_mapper_server(&pb_server).await;
    });
    // slepp some time to wait for pb server
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run subcribe server cli
    let key = server_key.clone();
    let subcribe_remote = pb_mapper_server.clone();
    let pb_mapper_server_cli_handle = tokio::spawn(async move {
        run_pb_mapper_server_cli(server_type, &echo_server, &subcribe_remote, &key, false).await;
    });
    // slepp some time to wait for pb server cli
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run register client cli
    let key = server_key.clone();
    let local_echo = local_server.clone();
    let register_remote = pb_mapper_server.clone();
    let pb_mapper_client_cli_handle = tokio::spawn(async move {
        run_pb_mapper_client_cli(server_type, &local_echo, &register_remote, &key).await;
    });
    // slepp some time to wait for pb client cli
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run echo test
    match server_type {
        ServerType::Udp => run_udp_datagram_echo(local_server.as_str(), 10, 8).await,
        ServerType::Tcp => run_echo_delay::<TcpStreamProvider, _>(local_server.as_str(), 10).await,
    }

    // abort all thread
    echo_server_handle.abort();
    pb_mapper_server_handle.abort();
    pb_mapper_server_cli_handle.abort();
    pb_mapper_client_cli_handle.abort();
}

/// This is only for testing the correctness of the logic, for performance testing of latency,
/// please run a separate binary.
#[tokio::test]
async fn test_pb_mapper_server_codec() {
    *INIT_TRACING;
    // run echo server
    let remote_echo = ECHO_SERVER.clone();
    let server_type = *SERVER_TYPE;
    let pb_mapper_server = PB_MAPPER_SERVER.clone();
    let server_key = SERVER_KEY.clone();
    let echo_server = ECHO_SERVER.clone();
    let local_server = LOCAL_SERVER.clone();

    let echo_server_handle =
        tokio::spawn(async move { run_echo_server(server_type, &remote_echo).await.unwrap() });
    // run pb-mapper-server
    let pb_server = pb_mapper_server.clone();
    let pb_mapper_server_handle = tokio::spawn(async move {
        run_pb_mapper_server(&pb_server).await;
    });
    // slepp some time to wait for pb server
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run subcribe server cli
    let key = server_key.clone();
    let subcribe_remote = pb_mapper_server.clone();
    let pb_mapper_server_cli_handle = tokio::spawn(async move {
        run_pb_mapper_server_cli(server_type, &echo_server, &subcribe_remote, &key, true).await;
    });
    // slepp some time to wait for pb server cli
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run register client cli
    let key = server_key.clone();
    let local_echo = local_server.clone();
    let register_remote = pb_mapper_server.clone();
    let pb_mapper_client_cli_handle = tokio::spawn(async move {
        run_pb_mapper_client_cli(server_type, &local_echo, &register_remote, &key).await;
    });
    // slepp some time to wait for pb client cli
    tokio::time::sleep(Duration::from_millis(200)).await;
    // run echo test
    match server_type {
        ServerType::Udp => run_udp_datagram_echo(local_server.as_str(), 10, 8).await,
        ServerType::Tcp => run_echo_delay::<TcpStreamProvider, _>(local_server.as_str(), 10).await,
    }

    // abort all thread
    echo_server_handle.abort();
    pb_mapper_server_handle.abort();
    pb_mapper_server_cli_handle.abort();
    pb_mapper_client_cli_handle.abort();
}

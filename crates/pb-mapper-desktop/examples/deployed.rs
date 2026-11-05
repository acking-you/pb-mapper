//! Acceptance against a configured deployed relay. Never print credentials.
//! The exported controller setup contains a dedicated expiring test credential,
//! never the administrator credential, and is revoked when the host exits.
use pb_mapper_client::sdk::{Client, ClientConfig, ConnectOptions, PathPolicy, RelayTransport};
use pb_mapper_desktop::{Host, HostOptions, VideoCodec, VideoPreferences, Viewer};
use serde::Deserialize;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Configuration {
    server_address: String,
    msg_header_key: String,
    #[serde(default)]
    relay_transport: RelayTransport,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let mut config: Configuration = serde_json::from_slice(&std::fs::read(
        args.get(1).ok_or("configuration path required")?,
    )?)?;
    if args.iter().any(|s| s == "--tcp") {
        config.relay_transport = RelayTransport::Tcp;
    }
    let output = PathBuf::from(args.get(2).ok_or("output directory required")?);
    let mode = args.get(3).map(String::as_str).unwrap_or("selftest");
    let seconds: u64 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(20);
    if !(5..=1800).contains(&seconds) {
        return Err("duration outside 5..1800 seconds".into());
    }
    let mut preferences = VideoPreferences::default();
    for (name, value) in [
        ("--max-width", &mut preferences.max_width),
        ("--max-height", &mut preferences.max_height),
        ("--fps", &mut preferences.max_fps),
        ("--bitrate", &mut preferences.bitrate),
    ] {
        if let Some(pair) = args.windows(2).find(|pair| pair[0] == name) {
            *value = pair[1].parse()?;
        }
    }
    preferences.validate()?;
    std::fs::create_dir_all(&output)?;
    let admin_client = Client::new(ClientConfig {
        server: config.server_address.clone(),
        credential: config.msg_header_key,
        keep_alive: true,
        namespace: None,
    })?
    .with_relay_transport(config.relay_transport)?;
    let admin = admin_client.admin()?;
    let key = admin
        .issue_key(
            Duration::from_secs(seconds + 300),
            Some("desktop-acceptance-ephemeral".into()),
        )
        .await?;
    let result = async {
        let client = Client::new(ClientConfig { server: config.server_address.clone(), credential: key.credential.clone(), keep_alive: true, namespace: None })?.with_relay_transport(config.relay_transport)?;
        let display = pb_mapper_desktop::displays()?.into_iter().next().ok_or("no display")?;
        let host = Host::start(&client, HostOptions { service: format!("desktop:acceptance-{}",std::process::id()), display_id:display.id, allow_control:false,allow_direct:true }).await?;
        let controller = serde_json::json!({"server":config.server_address,"credential":key.credential,"transport":config.relay_transport,"invitation":host.invitation().encode()?});
        std::fs::write(output.join("controller.json"),serde_json::to_vec(&controller)?)?;
        println!("host_ready display={}x{} duration={seconds} mode={mode}", display.width,display.height);
        if mode == "serve" {
            for _ in 0..seconds {
                if output.join("stop").exists() { break; }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let s=host.status();
                println!("state={} codec={:?} unique_capture_fps={:.1} frames={} dropped={} message={}",s.state,s.video.codec,s.capture_fps,s.frames,s.dropped_frames,s.message);
            }
        } else {
            for policy in [PathPolicy::Auto,PathPolicy::RelayOnly] {
                let viewer = Viewer::connect_with_video_preferences(&client,host.invitation().clone(),ConnectOptions { path: policy, ..Default::default() },vec![VideoCodec::H264],preferences).await?;
                let mut changes=viewer.frames();
                let start=Instant::now(); let mut received=0u64; let mut bytes=0u64; let mut path=String::new();
                while start.elapsed()<Duration::from_secs(seconds) {
                    if let Some(frame)=viewer.next_frame() {
                        if frame.status.state=="capture_unavailable" { return Err(frame.status.message.into()); }
                        path=frame.status.path.clone();
                        if !frame.status.video.duplicate {received+=1;}
                        bytes+=frame.bytes.len() as u64;
                    } else { tokio::time::timeout(Duration::from_secs(5),changes.changed()).await??; }
                }
                let elapsed=start.elapsed().as_secs_f64(); let status=viewer.status();
                println!("path={path} video={}x{} fps_limit={} unique_receive_fps={:.2} source_fps={:.2} receive_mbps={:.2} dropped={}", status.video.width,status.video.height,preferences.max_fps,received as f64/elapsed,status.capture_fps, bytes as f64*8.0/elapsed/1e6,status.dropped_frames);
                viewer.stop().await?;
            }
        }
        host.stop().await?;
        Ok::<_,Box<dyn std::error::Error>>(())
    }.await;
    let revoke = admin.revoke_key(key.key_id).await;
    let _ = std::fs::remove_file(output.join("controller.json"));
    revoke?;
    result
}

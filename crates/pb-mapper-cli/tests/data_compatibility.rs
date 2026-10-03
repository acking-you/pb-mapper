//! Mixed data formats on a real relay; old peers omit capability fields.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use pb_mapper_core::checksum::Credential;
use pb_mapper_protocol::command::{LocalServer, MessageSerializer, PbConnRequest, PbConnResponse};
use pb_mapper_protocol::data::DataCodec;
use pb_mapper_protocol::secure::ClientHeaderSession;
use pb_mapper_protocol::{CodecMessageReader, CodecMessageWriter, MessageReader, MessageWriter};
use pb_mapper_testkit::{Relay, admin_key_bytes};
use tokio::net::TcpStream;

#[tokio::test]
async fn old_and_new_data_peers_mix_independently_on_tcp_and_udp() {
    for datagram in [false, true] {
        for subscriber_v2 in [false, true] {
            for publisher_v2 in [false, true] {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    check_pair(datagram, subscriber_v2, publisher_v2),
                )
                .await
                .unwrap();
            }
        }
    }
}

async fn check_pair(datagram: bool, subscriber_v2: bool, publisher_v2: bool) {
    let relay = Relay::start("mixed-data").await;
    let credential = Credential::Admin(admin_key_bytes());
    let mut control = TcpStream::connect(relay.addr()).await.unwrap();
    let control_session = ClientHeaderSession::new_v2(&credential).unwrap();
    control_session
        .write_initial(
            &mut control,
            &PbConnRequest::Register {
                need_codec: true,
                is_datagram: datagram,
                key: "mixed".into(),
                protocol_version: Some(2),
                client_instance_id: Some("mixed-data-test".into()),
                heartbeat_interval_ms: Some(5_000),
                heartbeat_tolerance_ms: Some(15_000),
            }
            .encode()
            .unwrap(),
        )
        .await
        .unwrap();
    let mut control_reader = control_session.response_reader(&mut control).unwrap();
    assert!(matches!(
        PbConnResponse::decode(control_reader.read_msg().await.unwrap()).unwrap(),
        PbConnResponse::RegisterV2 { .. }
    ));

    let mut subscriber = TcpStream::connect(relay.addr()).await.unwrap();
    let subscriber_session = ClientHeaderSession::new_v2(&credential).unwrap();
    // These fixtures are the historical JSON shape when the peer is old.
    let mut request = serde_json::json!({"Subcribe": {"key": "mixed"}});
    if subscriber_v2 {
        request["Subcribe"]["data_protocol"] = 2.into();
    }
    subscriber_session
        .write_initial(&mut subscriber, &serde_json::to_vec(&request).unwrap())
        .await
        .unwrap();
    let LocalServer::Stream {
        client_id,
        server_generation,
    } = LocalServer::decode(control_reader.read_msg().await.unwrap()).unwrap()
    else {
        panic!("expected stream request");
    };

    let mut publisher = TcpStream::connect(relay.addr()).await.unwrap();
    let publisher_session = ClientHeaderSession::new_v2(&credential).unwrap();
    let mut request = serde_json::json!({"Stream": {"key": "mixed", "dst_id": client_id, "server_generation": server_generation}});
    if publisher_v2 {
        request["Stream"]["data_protocol"] = 2.into();
    }
    publisher_session
        .write_initial(&mut publisher, &serde_json::to_vec(&request).unwrap())
        .await
        .unwrap();

    let subscriber_codec =
        read_selection(&mut subscriber, &subscriber_session, subscriber_v2).await;
    let publisher_codec = read_selection(&mut publisher, &publisher_session, publisher_v2).await;
    assert_ne!(
        subscriber_codec.key(),
        publisher_codec.key(),
        "relay legs must never share a root data key"
    );
    let (subscriber_decode, subscriber_encode) = subscriber_codec.endpoint_codecs().unwrap();
    let (publisher_decode, publisher_encode) = publisher_codec.endpoint_codecs().unwrap();
    let (mut subscriber_read, mut subscriber_write) = subscriber.split();
    let (mut publisher_read, mut publisher_write) = publisher.split();
    let mut subscriber_reader = CodecMessageReader::new(&mut subscriber_read, subscriber_decode)
        .with_checksum_key(admin_key_bytes());
    let mut subscriber_writer = CodecMessageWriter::new(&mut subscriber_write, subscriber_encode)
        .with_checksum_key(admin_key_bytes());
    let mut publisher_reader = CodecMessageReader::new(&mut publisher_read, publisher_decode)
        .with_checksum_key(admin_key_bytes());
    let mut publisher_writer = CodecMessageWriter::new(&mut publisher_write, publisher_encode)
        .with_checksum_key(admin_key_bytes());
    for index in 0..8 {
        let up = vec![index; 117 + usize::from(index)];
        let down = vec![index + 8; 231 + usize::from(index)];
        let (a, b) = tokio::join!(
            subscriber_writer.write_msg(&up),
            publisher_writer.write_msg(&down)
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(publisher_reader.read_msg().await.unwrap(), up);
        assert_eq!(subscriber_reader.read_msg().await.unwrap(), down);
    }
}

async fn read_selection(
    stream: &mut TcpStream,
    session: &ClientHeaderSession,
    offered: bool,
) -> DataCodec {
    let mut reader = session.response_reader(stream).unwrap();
    let bytes = reader.read_msg().await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let selected = json.as_object().unwrap().values().next().unwrap();
    if !offered {
        assert!(selected.get("data_protocol").is_none());
    }
    let (key, version) = match PbConnResponse::decode(bytes).unwrap() {
        PbConnResponse::Stream {
            codec_key,
            data_protocol,
        }
        | PbConnResponse::Subcribe {
            codec_key,
            data_protocol,
            ..
        } => (codec_key, data_protocol),
        _ => panic!("expected data response"),
    };
    assert_eq!(version, offered.then_some(2));
    DataCodec::from_response(key, version, offered)
        .unwrap()
        .unwrap()
}

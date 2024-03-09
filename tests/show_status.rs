use pb_mapper::common::message::PbConnStatusReq;
use pb_mapper::local::client::show_status;

#[tokio::test]
async fn show_status_test() {
    show_status("127.0.0.1:11111", PbConnStatusReq::RemoteId).await;
}

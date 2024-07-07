use serde::{Deserialize, Serialize};
use snafu::ResultExt;

use super::super::error::{MsgSerializeSnafu, Result};

pub trait MessageSerializer {
    fn encode(&self) -> Result<Vec<u8>>;
    fn decode(msg: &[u8]) -> Result<Self>
    where
        Self: Sized;
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum PbConnStatusReq {
    RemoteId,
    Keys,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnStatusResp {
    RemoteId {
        server_map: String,
        active: String,
        idle: String,
    },
    Keys(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub enum PbConnRequest {
    Register { key: String },
    Subcribe { key: String },
    Status(PbConnStatusReq),
    Stream { key: String, dst_id: u32 },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbConnResponse {
    Register(u32),
    Subcribe { client_id: u32, server_id: u32 },
    Stream,
    Status(PbConnStatusResp),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum PbServerRequest {
    Ping,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub enum LocalServer {
    /// pb server makes a stream request to local server
    Stream { client_id: u32 },
    /// pb server response a pong msg when it receive a ping request
    Pong,
}

const CONTENT_SHOW_LIMIT_SIZE: usize = 1024;

#[inline]
fn get_content(raw_content: String) -> String {
    if raw_content.len() > CONTENT_SHOW_LIMIT_SIZE {
        raw_content[0..CONTENT_SHOW_LIMIT_SIZE].to_string()
    } else {
        raw_content
    }
}

macro_rules! gen_impl_msg_serializer {
    ($struct_name:ident) => {
        impl MessageSerializer for $struct_name {
            fn encode(&self) -> Result<Vec<u8>> {
                serde_json::to_vec(self).with_context(|_| MsgSerializeSnafu {
                    action: "encode",
                    struct_name: stringify!($struct_name),
                    content: get_content(format!("{self:?}")),
                })
            }

            fn decode(msg: &[u8]) -> Result<Self> {
                serde_json::from_slice(msg).with_context(|_| MsgSerializeSnafu {
                    action: "decode",
                    struct_name: stringify!($struct_name),
                    content: get_content(format!("{}", String::from_utf8_lossy(msg))),
                })
            }
        }
    };
}

gen_impl_msg_serializer!(PbConnRequest);
gen_impl_msg_serializer!(PbConnResponse);
gen_impl_msg_serializer!(PbServerRequest);
gen_impl_msg_serializer!(LocalServer);

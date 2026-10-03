//! Optional data encryption, negotiated independently on each relay leg.
//! The authenticated setup response selects a format before application I/O.

use pb_mapper_core::checksum::AesKeyType;
use pb_mapper_core::codec::{Aes256GcmDeCodec, Aes256GcmEnCodec};
use pb_mapper_core::error::Result;
use ring::hkdf::{HKDF_SHA256, KeyType, Salt};

use crate::secure::{HeaderProtocol, protocol_error};

/// Version with distinct directional keys and checked 64-bit nonce counters.
pub const DATA_PROTOCOL_V2: u16 = 2;

/// Negotiated key material for exactly one endpoint-to-relay connection.
/// Debug output intentionally never includes the secret.
#[derive(Clone, Copy)]
pub struct DataCodec {
    key: AesKeyType,
    protocol: Option<u16>,
}

impl DataCodec {
    /// Wrap an existing legacy key without changing its wire encoding.
    pub fn legacy(key: AesKeyType) -> Self {
        Self {
            key,
            protocol: None,
        }
    }

    /// Validate an authenticated response. Unknown selections are errors and
    /// must never cause another connection in a weaker mode.
    pub fn from_response(
        key: Option<AesKeyType>,
        protocol: Option<u16>,
        offered_v2: bool,
    ) -> Result<Option<Self>> {
        if protocol.is_some()
            && (protocol != Some(DATA_PROTOCOL_V2) || !offered_v2 || key.is_none())
        {
            return Err(protocol_error("invalid data protocol selection"));
        }
        Ok(key.map(|key| Self { key, protocol }))
    }

    /// Select only a capability received through authenticated control-v2.
    pub fn negotiate(key: AesKeyType, offer: Option<u16>, header: HeaderProtocol) -> Result<Self> {
        Self::validate_offer(offer, header)?;
        Ok(Self {
            key,
            protocol: offer,
        })
    }

    /// Reject unknown or unauthenticated selections before any setup response.
    pub fn validate_offer(offer: Option<u16>, header: HeaderProtocol) -> Result<()> {
        if offer.is_some() && (offer != Some(DATA_PROTOCOL_V2) || header != HeaderProtocol::V2) {
            return Err(protocol_error("unsupported data protocol offer"));
        }
        Ok(())
    }

    /// The per-leg secret sent inside the authenticated setup response.
    pub fn key(self) -> AesKeyType {
        self.key
    }

    /// Omit the field completely when interoperating with a legacy peer.
    pub fn protocol(self) -> Option<u16> {
        self.protocol
    }

    /// Receiving and sending codecs from the local endpoint's perspective.
    pub fn endpoint_codecs(self) -> Result<(Aes256GcmDeCodec, Aes256GcmEnCodec)> {
        self.codecs(false)
    }

    /// Receiving and sending codecs from the relay's perspective.
    pub fn relay_codecs(self) -> Result<(Aes256GcmDeCodec, Aes256GcmEnCodec)> {
        self.codecs(true)
    }

    fn codecs(self, relay: bool) -> Result<(Aes256GcmDeCodec, Aes256GcmEnCodec)> {
        if self.protocol.is_none() {
            return Ok((
                crate::get_decodec(&self.key)?,
                crate::get_encodec(&self.key)?,
            ));
        }
        let to_relay = self.derive(b"pb-mapper-data-v2-endpoint-to-relay")?;
        let to_endpoint = self.derive(b"pb-mapper-data-v2-relay-to-endpoint")?;
        let (read, write) = if relay {
            (to_relay, to_endpoint)
        } else {
            (to_endpoint, to_relay)
        };
        Ok((
            Aes256GcmDeCodec::try_new_data_v2(&read)
                .map_err(|_| protocol_error("invalid data-v2 read key"))?,
            Aes256GcmEnCodec::try_new_data_v2(&write)
                .map_err(|_| protocol_error("invalid data-v2 write key"))?,
        ))
    }

    fn derive(self, label: &'static [u8]) -> Result<AesKeyType> {
        struct KeyLen;
        impl KeyType for KeyLen {
            fn len(&self) -> usize {
                32
            }
        }
        let prk = Salt::new(HKDF_SHA256, b"pb-mapper-data-v2").extract(&self.key);
        let info = [label];
        let okm = prk
            .expand(&info, KeyLen)
            .map_err(|_| protocol_error("data key derivation failed"))?;
        let mut key = [0; 32];
        okm.fill(&mut key)
            .map_err(|_| protocol_error("data key derivation failed"))?;
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{MessageSerializer, PbConnRequest, PbConnResponse};
    use pb_mapper_core::codec::{Decryptor, Encryptor};

    fn seal(codec: &mut Aes256GcmEnCodec, bytes: &[u8]) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        let tag = codec.encrypt(&mut bytes).unwrap();
        bytes.extend_from_slice(tag.as_ref());
        bytes
    }

    #[test]
    fn data_keys_separate_both_directions_and_both_relay_legs() {
        let mut ciphertexts = Vec::new();
        for secret in [[41; 32], [42; 32]] {
            let codec = DataCodec::negotiate(secret, Some(2), HeaderProtocol::V2).unwrap();
            let (mut endpoint_read, mut endpoint_write) = codec.endpoint_codecs().unwrap();
            let (mut relay_read, mut relay_write) = codec.relay_codecs().unwrap();
            let first = seal(&mut endpoint_write, b"same bytes");
            let second = seal(&mut relay_write, b"same bytes");
            assert_eq!(
                relay_read.decrypt(&mut first.clone()).unwrap(),
                b"same bytes"
            );
            assert_eq!(
                endpoint_read.decrypt(&mut second.clone()).unwrap(),
                b"same bytes"
            );
            // A replay is not the next frame in this direction.
            assert!(relay_read.decrypt(&mut first.clone()).is_err());
            ciphertexts.extend([first, second]);
        }
        for (i, left) in ciphertexts.iter().enumerate() {
            for right in &ciphertexts[i + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn data_selection_requires_an_authenticated_supported_offer() {
        assert!(DataCodec::negotiate([0; 32], Some(2), HeaderProtocol::Legacy).is_err());
        assert!(DataCodec::negotiate([0; 32], Some(3), HeaderProtocol::V2).is_err());
        for (key, selected, offered) in [
            (None, Some(2), true),
            (Some([0; 32]), Some(3), true),
            (Some([0; 32]), Some(2), false),
        ] {
            assert!(DataCodec::from_response(key, selected, offered).is_err());
        }
        let old = DataCodec::from_response(Some([1; 32]), None, true)
            .unwrap()
            .unwrap();
        assert_eq!(old.protocol(), None);
        assert!(
            DataCodec::from_response(None, None, true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_json_ignores_offers_and_new_readers_accept_absent_selections() {
        #[derive(serde::Deserialize)]
        enum OldRequest {
            Subcribe { key: String },
            Stream { key: String, dst_id: u32 },
        }
        for request in [
            PbConnRequest::Subcribe {
                key: "compat".into(),
                data_protocol: Some(2),
            },
            PbConnRequest::Stream {
                key: "compat".into(),
                dst_id: 7,
                server_generation: 0,
                data_protocol: Some(2),
            },
        ] {
            match serde_json::from_slice::<OldRequest>(&request.encode().unwrap()).unwrap() {
                OldRequest::Subcribe { key } => assert_eq!(key, "compat"),
                OldRequest::Stream { key, dst_id } => {
                    assert_eq!(key, "compat");
                    assert_eq!(dst_id, 7);
                }
            }
        }
        let response = PbConnResponse::decode(br#"{"Stream":{"codec_key":null}}"#).unwrap();
        assert!(matches!(
            response,
            PbConnResponse::Stream {
                codec_key: None,
                data_protocol: None
            }
        ));
        let request = PbConnRequest::decode(br#"{"Subcribe":{"key":"compat"}}"#).unwrap();
        assert!(matches!(
            request,
            PbConnRequest::Subcribe {
                data_protocol: None,
                ..
            }
        ));
        let response = PbConnResponse::Stream {
            codec_key: None,
            data_protocol: None,
        }
        .encode()
        .unwrap();
        assert_eq!(response, br#"{"Stream":{"codec_key":null}}"#);
    }
}

//! The requests the facade sends the actor.
use super::super::*;

pub(crate) enum AuthCommand {
    ClaimAdminMutation {
        authority: Weak<AuthLease>,
        fingerprint: [u8; 32],
        client_timestamp: u64,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    AllocateNamespace {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<u64, AuthFailure>>,
    },
    Issue {
        authority: Weak<AuthLease>,
        namespace: Option<u64>,
        ttl: Duration,
        label: Option<String>,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    List {
        authority: Weak<AuthLease>,
        page: u32,
        page_size: u16,
        response: oneshot::Sender<Result<KeyPage, AuthFailure>>,
    },
    Show {
        authority: Weak<AuthLease>,
        key_id: KeyId,
        reveal: bool,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Renew {
        authority: Weak<AuthLease>,
        key_id: KeyId,
        ttl: Duration,
        response: oneshot::Sender<Result<IssuedTemporaryKey, AuthFailure>>,
    },
    Revoke {
        authority: Weak<AuthLease>,
        key_id: KeyId,
        response: oneshot::Sender<Result<TemporaryKeyMetadata, AuthFailure>>,
    },
    Gc {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<u64, AuthFailure>>,
    },
    Reset {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    RotateRoot {
        authority: Weak<AuthLease>,
        new_key: AesKeyType,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    SetLegacyProtocol {
        authority: Weak<AuthLease>,
        policy: LegacyProtocolPolicy,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Status {
        authority: Weak<AuthLease>,
        response: oneshot::Sender<Result<AuthStatus, AuthFailure>>,
    },
    Audit {
        authority: Weak<AuthLease>,
        action: String,
        key_id: Option<KeyId>,
        detail: Option<String>,
        response: oneshot::Sender<Result<(), AuthFailure>>,
    },
    Shutdown {
        response: oneshot::Sender<()>,
    },
}

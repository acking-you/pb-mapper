//! The lease an authenticated connection holds, and why it ended.
use super::*;

/// Why a lease was cancelled, so a dead credential can say why. Stored in an
/// `AtomicU8`; the first reason recorded wins.
#[repr(u8)]
#[derive(Clone, Copy)]
enum CancelReason {
    /// Not cancelled, or cancelled without a recorded reason.
    None = 0,
    Expired = 1,
    Revoked = 2,
    Rotated = 3,
}

impl CancelReason {
    fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Expired,
            2 => Self::Revoked,
            3 => Self::Rotated,
            _ => Self::None,
        }
    }
}

#[derive(Debug)]
pub struct AuthLease {
    key_id: KeyId,
    namespace: u64,
    expires_at: AtomicU64,
    cancellation: CancellationToken,
    /// A [`CancelReason`].
    cancel_reason: AtomicU8,
}

impl AuthLease {
    pub(crate) fn new(key_id: KeyId, issued_into: Option<u64>, expires_at: u64) -> Self {
        Self {
            key_id,
            namespace: key_id.namespace(issued_into),
            expires_at: AtomicU64::new(expires_at),
            cancellation: CancellationToken::new(),
            cancel_reason: AtomicU8::new(CancelReason::None as u8),
        }
    }

    pub fn key_id(&self) -> KeyId {
        self.key_id
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at.load(Ordering::Acquire)
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn record_cancel(&self, reason: CancelReason) {
        let _ = self.cancel_reason.compare_exchange(
            CancelReason::None as u8,
            reason as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.cancellation.cancel();
    }

    fn cancel_reason(&self) -> CancelReason {
        CancelReason::from_u8(self.cancel_reason.load(Ordering::Acquire))
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Moves the deadline `ensure_active` checks, for a renewal.
    pub(crate) fn set_expires_at(&self, expires_at: u64) {
        self.expires_at.store(expires_at, Ordering::Release);
    }

    pub(crate) fn cancel_expired(&self) {
        self.record_cancel(CancelReason::Expired);
    }

    pub(crate) fn cancel_revoked(&self) {
        self.record_cancel(CancelReason::Revoked);
    }

    pub(crate) fn cancel_rotated(&self) {
        self.record_cancel(CancelReason::Rotated);
    }
}

#[derive(Clone, Debug)]
pub struct AuthContext {
    pub key_id: KeyId,
    pub namespace: u64,
    pub is_admin: bool,
    lease: Weak<AuthLease>,
}

impl AuthContext {
    pub(crate) fn from_lease(key_id: KeyId, is_admin: bool, lease: &Arc<AuthLease>) -> Self {
        Self {
            key_id,
            namespace: lease.namespace,
            is_admin,
            lease: Arc::downgrade(lease),
        }
    }

    pub fn ensure_active(&self) -> Result<Arc<AuthLease>, AuthFailure> {
        let lease = self.lease.upgrade().ok_or_else(|| {
            AuthFailure::new(
                if self.is_admin {
                    codes::ADMINISTRATOR_KEY_ROTATED
                } else {
                    codes::TEMPORARY_KEY_INACTIVE
                },
                "credential lease is no longer active",
                false,
            )
        })?;
        if lease.is_cancelled() {
            return Err(cancelled_lease_failure(self.is_admin, &lease));
        }
        if !self.is_admin && lease.expires_at() <= unix_seconds() {
            lease.cancel_expired();
            return Err(AuthFailure::key_expired());
        }
        Ok(lease)
    }

    pub fn cancellation_token(&self) -> Result<CancellationToken, AuthFailure> {
        Ok(self.ensure_active()?.cancellation_token())
    }

    pub fn admin_cancellation_token(&self) -> Result<CancellationToken, AuthFailure> {
        self.require_admin()?;
        self.cancellation_token()
    }

    pub(crate) fn admin_authority(&self) -> Result<Weak<AuthLease>, AuthFailure> {
        self.require_admin()?;
        self.ensure_active()?;
        Ok(self.lease.clone())
    }

    fn require_admin(&self) -> Result<(), AuthFailure> {
        if self.is_admin {
            Ok(())
        } else {
            Err(AuthFailure::new(
                codes::ADMIN_PERMISSION_REQUIRED,
                "administrator credential is required for this operation",
                false,
            ))
        }
    }
}

fn cancelled_lease_failure(is_admin: bool, lease: &AuthLease) -> AuthFailure {
    if is_admin {
        return AuthFailure::new(
            codes::ADMINISTRATOR_KEY_ROTATED,
            "credential lease has been cancelled",
            false,
        );
    }
    match lease.cancel_reason() {
        CancelReason::Expired => AuthFailure::key_expired(),
        CancelReason::Rotated => AuthFailure::key_rotated(),
        CancelReason::Revoked => AuthFailure::key_revoked(),
        CancelReason::None => AuthFailure::new(
            codes::TEMPORARY_KEY_INACTIVE,
            "credential lease has been cancelled",
            false,
        ),
    }
}

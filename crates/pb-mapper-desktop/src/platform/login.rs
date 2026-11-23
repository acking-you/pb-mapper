//! What the Windows background service asks Windows, on each of its ticks,
//! about its owner's console login. Attesting the login in full takes the
//! login's token and compares its SID: several calls into the session
//! manager and LSA. Rechecking a login attested before is one session query,
//! which compares its session, logon time, user and domain. The console's
//! session id, read from memory the kernel shares, shows a change of hands
//! for nothing. Pure, so every platform tests it.

use std::time::Duration;

/// How long the service trusts the login it attested, while the console's
/// session id is unchanged, before it rechecks it. Only a logoff followed by
/// a new login at the same session id goes unseen that long; a capture never
/// relies on it, since it attests the login in full when it starts and every
/// 100 ms while it runs.
pub(crate) const RECHECK: Duration = Duration::from_secs(5);

/// What a tick asks Windows about the owner's login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attest {
    /// Attest whose login holds the console: its token and SID.
    Full,
    /// Ask whether the login attested before still holds the console.
    Recheck,
    /// Ask nothing: the login attested before holds the console still.
    Trust,
}

/// What a tick asks, given the owner's login attested before (`attested`:
/// its session id, and how long ago it was attested or last rechecked), the
/// console's session id now (`active`) and whether this tick may start the
/// host (`starting`). Nothing starts the host on trust: a start attests the
/// login in full, as every tick once did.
pub(crate) fn attest(attested: Option<(u32, Duration)>, active: u32, starting: bool) -> Attest {
    match attested {
        Some((session, since)) if !starting && session == active => {
            if since >= RECHECK {
                Attest::Recheck
            } else {
                Attest::Trust
            }
        }
        Some(_) | None => Attest::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The login is attested in full when nothing was, when the console
    /// changes hands and before a start; between those, it is rechecked once
    /// `RECHECK` has passed, and trusted until then.
    #[test]
    fn a_login_is_attested_in_full_only_when_the_console_changes_hands_or_the_host_starts() {
        let just = Duration::ZERO;
        assert_eq!(attest(None, 1, false), Attest::Full);
        assert_eq!(attest(None, 1, true), Attest::Full);
        assert_eq!(attest(Some((1, just)), 1, false), Attest::Trust);
        assert_eq!(
            attest(Some((1, RECHECK - Duration::from_millis(1))), 1, false),
            Attest::Trust
        );
        assert_eq!(attest(Some((1, RECHECK)), 1, false), Attest::Recheck);
        // Another session holds the console: a switch, a remote connection
        // or a new login.
        assert_eq!(attest(Some((1, just)), 2, false), Attest::Full);
        assert_eq!(attest(Some((1, RECHECK)), 2, false), Attest::Full);
        // A tick that may start the host never starts it on trust.
        assert_eq!(attest(Some((1, just)), 1, true), Attest::Full);
        assert_eq!(attest(Some((1, RECHECK)), 1, true), Attest::Full);
    }
}

//! The approved console login: whether one is active, who it is, and
//! whether it is still the same login.
use super::SessionIdentity;
use crate::{Result, error, platform::windows::win32::session_info};
use windows::Win32::System::RemoteDesktop::*;
#[cfg(feature = "windows-helper")]
use {
    super::{
        Handle,
        identity::{ScopedPrivileges, sid},
    },
    crate::platform::windows::win32::from_wide_nul,
    windows::Win32::{Foundation::HANDLE, Security::SE_TCB_NAME},
};

pub fn active_session() -> u32 {
    unsafe { WTSGetActiveConsoleSessionId() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleState {
    Locked,
    Unlocked,
}

#[cfg(feature = "windows-helper")]
pub fn background_session(owner_sid: &str) -> Result<Option<u32>> {
    Ok(available_owner_session(owner_sid)?.map(|identity| identity.session))
}

#[cfg(feature = "windows-helper")]
pub fn owner_session(owner_sid: &str) -> Result<SessionIdentity> {
    available_owner_session(owner_sid)?
        .ok_or_else(|| error("Waiting for the approved user to sign in to the active console"))
}

/// An absent/different login is ordinary waiting. OS query/privilege failures
/// are errors, never evidence that the installation owner has logged out.
#[cfg(feature = "windows-helper")]
fn available_owner_session(owner_sid: &str) -> Result<Option<SessionIdentity>> {
    let session = active_session();
    if session == 0 || session == u32::MAX {
        return Ok(None);
    }
    let Some((identity, _)) = session_state(session, &console_info(session)?) else {
        return Ok(None);
    };
    // WTSQueryUserToken requires LocalSystem/SeTcbPrivilege. Limit that
    // privilege to this synchronous query, restoring it before any await.
    let privileges = ScopedPrivileges::new(&[SE_TCB_NAME])
        .map_err(|e| error(format!("Cannot query the approved Windows login: {e}")))?;
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }
        .map_err(|e| error(format!("WTSQueryUserToken failed: {e}")))?;
    let token = unsafe { Handle::own(token) };
    if sid(token.raw())? != owner_sid {
        return Ok(None);
    }
    drop(privileges);
    identity.check()?;
    Ok(Some(identity))
}

fn console_info(session: u32) -> Result<WTSINFOEX_LEVEL1_W> {
    if session == 0 || session == u32::MAX || session != active_session() {
        return Err(error("Only the active console session is supported"));
    }
    session_info(session)?.ok_or_else(|| error("Console session information is unavailable"))
}

fn console_state(session: u32, info: &WTSINFOEX_LEVEL1_W) -> Option<ConsoleState> {
    if session == 0
        || session == u32::MAX
        || info.SessionId != session
        || info.SessionState != WTSActive
        || info.UserName[0] == 0
        || info.LogonTime <= 0
    {
        return None;
    }
    match info.SessionFlags as u32 {
        WTS_SESSIONSTATE_LOCK => Some(ConsoleState::Locked),
        WTS_SESSIONSTATE_UNLOCK => Some(ConsoleState::Unlocked),
        _ => None,
    }
}

#[cfg(feature = "windows-helper")]
fn session_state(
    session: u32,
    info: &WTSINFOEX_LEVEL1_W,
) -> Option<(SessionIdentity, ConsoleState)> {
    let state = console_state(session, info)?;
    Some((
        SessionIdentity {
            session,
            logon_time: info.LogonTime,
            user: from_wide_nul(&info.UserName),
            domain: from_wide_nul(&info.DomainName),
        },
        state,
    ))
}

impl SessionIdentity {
    pub(in crate::platform::windows::helper) fn check(&self) -> Result<ConsoleState> {
        let info = console_info(self.session)?;
        let state = console_state(self.session, &info)
            .ok_or_else(|| error("The approved console login session is unavailable"))?;
        if !self.matches(&info) {
            return Err(error("The approved console login session has ended"));
        }
        Ok(state)
    }

    fn matches(&self, info: &WTSINFOEX_LEVEL1_W) -> bool {
        // Called on the capture/input path: compare existing UTF-16 slices,
        // allocating user/domain strings only when a login is first attested.
        let Self {
            session,
            logon_time,
            user,
            domain,
        } = self;
        *session == info.SessionId
            && *logon_time == info.LogonTime
            && user
                .encode_utf16()
                .eq(info.UserName.iter().copied().take_while(|c| *c != 0))
            && domain
                .encode_utf16()
                .eq(info.DomainName.iter().copied().take_while(|c| *c != 0))
    }
}

#[cfg(all(test, feature = "windows-helper"))]
mod tests {
    use super::*;
    #[test]
    fn console_identity_survives_lock_but_not_logoff_or_session_reuse() {
        let mut info = WTSINFOEX_LEVEL1_W {
            SessionId: 7,
            SessionState: WTSActive,
            SessionFlags: WTS_SESSIONSTATE_UNLOCK as i32,
            LogonTime: 100,
            ..Default::default()
        };
        assert!(session_state(7, &info).is_none());
        info.UserName[0] = u16::from(b'u');
        let (identity, state) = session_state(7, &info).unwrap();
        assert_eq!(state, ConsoleState::Unlocked);
        assert!(identity.matches(&info));
        assert!(session_state(8, &info).is_none());
        info.SessionState = WTSDisconnected;
        assert!(session_state(7, &info).is_none());
        info.SessionState = WTSActive;
        info.SessionFlags = WTS_SESSIONSTATE_LOCK as i32;
        assert_eq!(
            session_state(7, &info),
            Some((identity.clone(), ConsoleState::Locked))
        );
        // Even the same user logging in again at a reused session ID is a new
        // authorization boundary; the old worker must never follow it.
        info.LogonTime += 1;
        assert_ne!(session_state(7, &info).unwrap().0, identity);
        assert!(!identity.matches(&info));
        info.LogonTime -= 1;
        info.UserName[0] = u16::from(b'v');
        assert_ne!(session_state(7, &info).unwrap().0, identity);
        assert!(!identity.matches(&info));
        info.UserName[0] = 0;
        assert!(session_state(7, &info).is_none());
    }
}

//! Acquire a logind sleep lock on demand without linking a D-Bus client into
//! the relay. systemd-inhibit runs the command only after logind grants it.
use std::{io, process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};

pub(super) struct Inhibitor(Child);

// The command runs after logind grants the request. Its only input is our
// pipe, which closes even if pb-mapper crashes.
const HOLD_UNTIL_EOF: &str = "printf R && { read -r unused || :; }";

impl Inhibitor {
    pub(super) async fn acquire() -> io::Result<Self> {
        let mut command = Command::new("systemd-inhibit");
        command.args([
            "--what=sleep",
            "--mode=block",
            "--who=pb-mapper",
            "--why=Keep pb-mapper available for incoming connections",
            "--",
            "/bin/sh",
            "-c",
            HOLD_UNTIL_EOF,
        ]);
        Self::start(command).await
    }

    async fn start(mut command: Command) -> io::Result<Self> {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not start systemd-inhibit: {error}"),
                )
            })?;
        let mut guard = Self(child);
        let mut ready = guard
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing inhibitor output pipe"))?;
        let reply = tokio::time::timeout(Duration::from_secs(5), ready.read_u8())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "systemd-inhibit timed out"))?;
        match reply {
            Ok(b'R') => Ok(guard),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected output from systemd-inhibit",
            )),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(io::Error::other(
                "systemd-inhibit could not acquire a sleep lock. Check logind and the permission to inhibit sleep",
            )),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_helper_that_cannot_acquire_the_lock_fails_the_request() {
        let mut denied = Command::new("/bin/sh");
        denied.args(["-c", "exit 1"]);
        assert!(Inhibitor::start(denied).await.is_err());
    }

    #[tokio::test]
    async fn the_lock_holder_lives_until_its_owner_closes_the_pipe() {
        let mut shell = Command::new("/bin/sh");
        shell.args(["-c", HOLD_UNTIL_EOF]);
        let mut guard = Inhibitor::start(shell).await.unwrap();
        assert!(guard.0.try_wait().unwrap().is_none());
        drop(guard.0.stdin.take());
        let status = tokio::time::timeout(Duration::from_secs(5), guard.0.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}

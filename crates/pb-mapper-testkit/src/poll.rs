//! The one readiness loop every probe in this crate polls with.

use std::time::Duration;

use tokio::time::Instant;

/// Run `attempt` every `every` until it succeeds, for at most `within`.
///
/// Returns the last attempt's error once the time is up, so the caller's panic
/// says what was still wrong rather than only that time ran out.
pub(crate) async fn poll_until<F, Fut>(
    within: Duration,
    every: Duration,
    mut attempt: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let deadline = Instant::now() + within;
    let mut last_error = String::from("no attempt completed");
    while Instant::now() < deadline {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(every).await;
    }
    Err(last_error)
}

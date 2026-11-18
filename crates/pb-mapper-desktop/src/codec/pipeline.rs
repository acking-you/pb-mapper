//! What every fused capture-and-encode pipeline shares: when its output is
//! late or stalled, how often a static desktop is sent again, and which
//! failures are the codec's own. Neutral: Windows' Media Foundation encoder
//! and macOS' VideoToolbox session follow the same rules.

use crate::{BackendError, FailureKind, VideoCodec};
use std::time::{Duration, Instant};

/// An output that took longer than this is discarded, and output resumes at
/// the next IDR. A unit's time counts from when the encoder could start it:
/// see [`late`].
pub(crate) const LATE_OUTPUT: Duration = Duration::from_millis(250);
/// A pipeline that has submitted nothing this long after it was built is
/// stuck.
pub(crate) const FIRST_SUBMIT_WAIT: Duration = Duration::from_secs(3);
/// An encoder that has held a unit this long is stuck.
pub(crate) const OUTPUT_WAIT: Duration = Duration::from_secs(2);
const _: () = assert!(
    LATE_OUTPUT.as_millis() < OUTPUT_WAIT.as_millis(),
    "a late unit is discarded before the encoder counts as stalled"
);
/// A static desktop still submits a verified repeat this often, which keeps a
/// presented frame fresh enough to authorise input.
pub(crate) const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);

/// Whether an output collected at `now`, of a unit submitted at `submitted`,
/// is later than `LATE_OUTPUT`. The encoder serves units in order, so one
/// queued behind others starts no earlier than the output before it, at
/// `previous`. The wait behind them, which thread C's `ENCODER_DEPTH` sets, is
/// not lateness: measured from the submit, a throughput-limited encoder at
/// that depth would be late on every unit, its IDRs included, and its picture
/// would freeze.
pub(crate) fn late(submitted: Instant, previous: Option<Instant>, now: Instant) -> bool {
    let started = previous.map_or(submitted, |previous| previous.max(submitted));
    now.saturating_duration_since(started) > LATE_OUTPUT
}

/// An error of the hardware codec itself, rather than of the capture or the
/// scaler around it. An H.265 one is `EncoderFailed`: thread C then ends the
/// session, and this host offers H.265 no more. Its kind crosses the
/// helper pipe as it is.
pub(crate) fn codec_error(kind: VideoCodec, failure: std::io::Error) -> std::io::Error {
    if kind == VideoCodec::H265 {
        BackendError::new(
            FailureKind::EncoderFailed { codec: kind },
            failure.to_string(),
        )
        .into()
    } else {
        failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn a_unit_queued_behind_a_slow_encoder_is_not_late() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // A serial encoder takes 70 ms a unit. Thread C fills it at 60 fps,
        // then submits one as each comes out: from its submit, every unit
        // after the first four takes 280 ms.
        let mut queued: VecDeque<u64> = [0, 17, 33, 50].into();
        assert_eq!(queued.len(), crate::worker::ENCODER_DEPTH);
        let mut previous = None;
        for unit in 1..=20 {
            let out = 70 * unit;
            let submitted = queued.pop_front().unwrap();
            assert!(!late(at(submitted), previous, at(out)), "unit {unit}");
            previous = Some(at(out));
            queued.push_back(out);
        }
        // A stuck encoder is still late, behind another unit or not.
        let submitted = queued.pop_front().unwrap();
        assert!(late(at(submitted), previous, at(70 * 20 + 251)));
        assert!(late(at(5_000), previous, at(5_251)));
    }
}

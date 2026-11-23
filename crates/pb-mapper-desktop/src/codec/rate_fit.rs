//! Which rate model a pace sweep fits. An encoder set to a bitrate at a
//! nominal frame rate is paced lower, step by step. A `nominal` encoder
//! budgets each frame as its share of the bitrate at the nominal rate, so
//! its bitrate falls with the pace; a `timestamp` encoder budgets per second
//! of presentation time, so it holds the bitrate. The fit is the model the
//! produced bitrates lie closer to, as log ratios summed over the steps, and
//! it holds when every step lies within 25% of it.
//!
//! Measurement support, built for tests only and never shipped: the macOS
//! encoder's measurement uses it, and `examples/video_bench.rs` includes this
//! file by path for `--pace`, so it names nothing of this crate.

/// One step: what it produced against each model's expectation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub pace: u32,
    /// Bits a second, once the step settled.
    pub produced_bps: f64,
    /// What the nominal model expects: the bitrate × pace / nominal rate.
    pub nominal_bps: f64,
    /// Produced over the nominal model's expectation.
    pub nominal: f64,
    /// Produced over the bitrate, the timestamp model's expectation.
    pub timestamp: f64,
}

/// A sweep and the model it fits.
#[derive(Debug)]
pub(crate) struct Fit {
    pub steps: Vec<Step>,
    /// The nominal model fits at least as well as the timestamp one.
    pub nominal: bool,
    /// Every step lies within 25% of the model that fits.
    pub within: bool,
}

impl Fit {
    /// The sweep of an encoder set to `bitrate` at `nominal_fps`: each
    /// step's pace and the bitrate it produced once settled.
    pub(crate) fn new(bitrate: u32, nominal_fps: u32, sweep: &[(u32, f64)]) -> Self {
        let bitrate = f64::from(bitrate);
        let steps: Vec<Step> = sweep
            .iter()
            .map(|&(pace, produced_bps)| {
                let nominal_bps = bitrate * f64::from(pace) / f64::from(nominal_fps);
                Step {
                    pace,
                    produced_bps,
                    nominal_bps,
                    nominal: produced_bps / nominal_bps,
                    timestamp: produced_bps / bitrate,
                }
            })
            .collect();
        let score =
            |pick: fn(&Step) -> f64| steps.iter().map(|step| pick(step).ln().abs()).sum::<f64>();
        let nominal = score(|step| step.nominal) <= score(|step| step.timestamp);
        let within = steps
            .iter()
            .map(|step| {
                if nominal {
                    step.nominal
                } else {
                    step.timestamp
                }
            })
            .all(|ratio| (0.75..=1.25).contains(&ratio));
        Self {
            steps,
            nominal,
            within,
        }
    }

    /// The fitted model, as `Stats` names it.
    pub(crate) fn model(&self) -> &'static str {
        if self.nominal { "nominal" } else { "timestamp" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sweep whose bitrate follows the pace fits `nominal`, one that holds
    /// the bitrate fits `timestamp`, and one whose slowest step the content
    /// held 40% above its budget fits neither within 25%.
    #[test]
    fn a_sweep_fits_the_model_its_bitrate_follows() {
        let sweep = |produced: &dyn Fn(f64) -> f64| {
            let steps = [60, 30, 15, 5].map(|pace| (pace, produced(f64::from(pace))));
            Fit::new(4_000_000, 60, &steps)
        };
        let per_frame = sweep(&|pace| 4e6 * pace / 60.0);
        assert!(per_frame.nominal && per_frame.within, "{per_frame:?}");
        let per_second = sweep(&|_| 4.2e6);
        assert!(!per_second.nominal && per_second.within, "{per_second:?}");
        assert_eq!(per_second.model(), "timestamp");
        let floored = sweep(&|pace| 4e6 * pace.max(7.0) / 60.0);
        assert!(floored.nominal && !floored.within, "{floored:?}");
    }
}

//! Module: An opt-in pace for work that would otherwise run as fast as it can.
//! Correctness: Correct when an unset limit costs nothing, when a configured
//! limit paces to the rate asked for, and when a limit that cannot be honoured
//! is reported instead of being quietly replaced by a default.
//! Last revised: 2026-08-25
//! Last changed: New.
//!
//! # Why default unlimited
//!
//! forge is a developer tool run against a checkout, and the common case is a
//! person waiting for an answer. Throttling that by default would make every
//! command slower to protect against a situation most runs are not in.
//!
//! The situation some runs ARE in: a scan across a large tree, on a laptop that
//! is also running a build, a cluster and an editor. There the useful knob is
//! not "how many threads" -- forge is mostly not thread-bound -- but how fast
//! it is allowed to keep starting new work.
//!
//! So the limit is off unless asked for, and asking for it is one number.
//!
//! # Why a pace rather than a token bucket
//!
//! A bucket permits a burst, which is the right shape when protecting a remote
//! service from a spike. This protects the LOCAL machine from sustained load,
//! where a burst is exactly what hurts: the point is to leave the CPU idle
//! between units of work, not to average out over a window.

use std::time::Duration;

/// The environment variable that sets the limit.
pub const RATE_LIMIT_ENV: &str = "FORGE_MAX_OPS_PER_SEC";

/// How fast forge may start new units of work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RateLimit {
    /// The smallest gap between two units. `None` is unlimited.
    min_interval: Option<Duration>,
}

impl RateLimit {
    /// No pacing. The default, and what every command does today.
    pub fn unlimited() -> Self {
        Self { min_interval: None }
    }

    /// Pace to `ops` units per second.
    ///
    /// A rate that cannot be honoured is an ERROR, not a shrug. Zero would
    /// stall forever, and a caller that asked for zero has made a mistake it
    /// should hear about rather than watch as a hang.
    pub fn per_second(ops: f64) -> Result<Self, String> {
        if ops.is_nan() {
            return Err("rate is not a number".to_owned());
        }
        if ops <= 0.0 {
            return Err(format!("rate must be greater than zero, got {ops}"));
        }
        if ops.is_infinite() {
            return Ok(Self::unlimited());
        }
        Ok(Self {
            min_interval: Some(Duration::from_secs_f64(1.0 / ops)),
        })
    }

    /// True when nothing is being paced.
    pub fn is_unlimited(&self) -> bool {
        self.min_interval.is_none()
    }

    /// How long to wait before starting the next unit.
    ///
    /// Takes the elapsed time rather than reading a clock, so the pacing can be
    /// asserted without sleeping and without a fake clock: the decision is
    /// arithmetic and the caller owns the waiting.
    pub fn delay_before_next(&self, since_last: Duration) -> Duration {
        match self.min_interval {
            None => Duration::ZERO,
            Some(interval) => interval.saturating_sub(since_last),
        }
    }
}

/// Read the limit from the environment.
///
/// Returns the limit and, when the value could not be used, the reason. The
/// reason is returned rather than logged here so the caller decides where it
/// surfaces -- but it must surface: a limit someone set and forge ignored,
/// silently, is a machine still being hammered by a command that was told not
/// to.
pub fn from_env_value(raw: Option<&str>) -> (RateLimit, Option<String>) {
    let Some(raw) = raw else {
        return (RateLimit::unlimited(), None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return (
            RateLimit::unlimited(),
            Some(format!("{RATE_LIMIT_ENV} is set but empty")),
        );
    }
    if trimmed.eq_ignore_ascii_case("unlimited") || trimmed == "0.0/unlimited" {
        return (RateLimit::unlimited(), None);
    }
    match trimmed.parse::<f64>() {
        Err(error) => (
            RateLimit::unlimited(),
            Some(format!(
                "{RATE_LIMIT_ENV}={trimmed} is not a number: {error}"
            )),
        ),
        Ok(value) => match RateLimit::per_second(value) {
            Ok(limit) => (limit, None),
            Err(why) => (
                RateLimit::unlimited(),
                Some(format!("{RATE_LIMIT_ENV}={trimmed} unusable: {why}")),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default must cost nothing. Every command runs this path.
    #[test]
    fn an_unset_limit_never_waits() {
        let limit = RateLimit::unlimited();
        assert!(limit.is_unlimited());
        for elapsed in [
            Duration::ZERO,
            Duration::from_micros(1),
            Duration::from_secs(60),
        ] {
            assert_eq!(limit.delay_before_next(elapsed), Duration::ZERO);
        }
    }

    #[test]
    fn the_default_is_unlimited() {
        assert_eq!(RateLimit::default(), RateLimit::unlimited());
        assert!(RateLimit::default().is_unlimited());
    }

    #[test]
    fn a_configured_rate_paces_to_that_rate() {
        let limit = RateLimit::per_second(10.0).expect("10/sec is usable");
        assert_eq!(
            limit.delay_before_next(Duration::ZERO),
            Duration::from_millis(100)
        );
        assert_eq!(
            limit.delay_before_next(Duration::from_millis(40)),
            Duration::from_millis(60)
        );
    }

    /// Work that already took longer than the interval must not be penalised
    /// again, and the wait can never go negative.
    #[test]
    fn slow_work_is_not_paced_further() {
        let limit = RateLimit::per_second(10.0).expect("usable");
        assert_eq!(
            limit.delay_before_next(Duration::from_millis(100)),
            Duration::ZERO
        );
        assert_eq!(
            limit.delay_before_next(Duration::from_secs(5)),
            Duration::ZERO
        );
    }

    /// Rates below one per second are the interesting case for a big scan.
    #[test]
    fn a_fractional_rate_is_honoured() {
        let limit = RateLimit::per_second(0.5).expect("usable");
        assert_eq!(
            limit.delay_before_next(Duration::ZERO),
            Duration::from_secs(2)
        );
    }

    /// Zero would stall forever. A caller that asked for it made a mistake and
    /// should be told, not left watching a hang.
    #[test]
    fn a_rate_that_cannot_be_honoured_is_an_error() {
        assert!(RateLimit::per_second(0.0).is_err());
        assert!(RateLimit::per_second(-1.0).is_err());
        assert!(RateLimit::per_second(f64::NAN).is_err());
    }

    #[test]
    fn an_infinite_rate_is_just_unlimited() {
        assert!(RateLimit::per_second(f64::INFINITY)
            .expect("usable")
            .is_unlimited());
    }

    #[test]
    fn an_absent_variable_is_unlimited_and_says_nothing() {
        let (limit, problem) = from_env_value(None);
        assert!(limit.is_unlimited());
        assert_eq!(problem, None, "not configuring a limit is not a problem");
    }

    #[test]
    fn a_set_variable_is_honoured() {
        let (limit, problem) = from_env_value(Some("4"));
        assert_eq!(problem, None);
        assert_eq!(
            limit.delay_before_next(Duration::ZERO),
            Duration::from_millis(250)
        );
        let (spaced, _) = from_env_value(Some("  4  "));
        assert_eq!(
            spaced, limit,
            "surrounding space is not a configuration error"
        );
    }

    #[test]
    fn unlimited_can_be_said_explicitly() {
        for spelling in ["unlimited", "UNLIMITED", "Unlimited"] {
            let (limit, problem) = from_env_value(Some(spelling));
            assert!(limit.is_unlimited(), "{spelling}");
            assert_eq!(problem, None, "{spelling}");
        }
    }

    /// The case that matters most: someone asked for a limit and forge could
    /// not use the value. Falling back to unlimited is the only safe behaviour
    /// -- refusing to run would break a machine-wide setting -- but it MUST be
    /// reported, or the operator believes a limit is in force while their
    /// machine is hammered by a command that was told to be gentle.
    #[test]
    fn an_unusable_value_falls_back_but_is_reported() {
        for bad in ["0", "-3", "lots", "", "  ", "1e", "NaN"] {
            let (limit, problem) = from_env_value(Some(bad));
            assert!(limit.is_unlimited(), "{bad:?} should not pace");
            assert!(
                problem.is_some(),
                "{bad:?} was ignored silently, which is the failure this guards"
            );
            let text = problem.unwrap();
            assert!(
                text.contains(RATE_LIMIT_ENV),
                "the message must name the variable: {text}"
            );
        }
    }
}

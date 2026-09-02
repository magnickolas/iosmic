use std::time::Duration;

use anyhow::Result;

use crate::error::ConfigurationError;

pub const REQUESTED_PERIOD: Duration = Duration::from_millis(10);
pub const WRITE_ATTEMPT: Duration = Duration::from_millis(20);
pub const WRITE_EXPIRY_ATTEMPTS: u32 = 3;
pub const MAX_CORRECTION: f64 = 1.02;
pub const MAX_GROUP_DELAY: Duration = Duration::from_micros(250);

const TARGET_RANGE_MS: std::ops::RangeInclusive<u32> = 50..=500;
const MAXIMUM_RANGE_MS: std::ops::RangeInclusive<u32> = 100..=2000;

#[derive(Clone, Debug)]
pub struct PlayPolicy {
    pub target_buffer_ms: Option<u32>,
    pub default_target_buffer_ms: u32,
    pub maximum_buffer_ms: Option<u32>,
    pub alsa_buffer_ms: u32,
    pub starvation_reconnect_ms: u32,
}

impl PlayPolicy {
    pub fn derive_limits(
        &self,
        source_rate: u32,
        largest_packet_frames: usize,
        actual_period: Duration,
    ) -> Result<BufferLimits> {
        let period_source_frames = duration_to_frames_ceil(actual_period, source_rate);
        let default_target_frames =
            milliseconds_to_frames(self.default_target_buffer_ms, source_rate);

        let target_frames = if let Some(milliseconds) = self.target_buffer_ms {
            let frames = milliseconds_to_frames(milliseconds, source_rate);
            if frames < largest_packet_frames + period_source_frames {
                return Err(ConfigurationError::new(format!(
                    "--buffer={milliseconds} ms has no packet headroom: need at least {:.3} ms for {largest_packet_frames} decoded frames plus one render period",
                    frames_to_milliseconds(largest_packet_frames + period_source_frames, source_rate),
                ))
                .into());
            }
            frames
        } else {
            default_target_frames.max(largest_packet_frames + period_source_frames)
        };

        validate_derived_range("--buffer", target_frames, source_rate, TARGET_RANGE_MS)?;

        let maximum_frames = if let Some(milliseconds) = self.maximum_buffer_ms {
            let frames = milliseconds_to_frames(milliseconds, source_rate);
            if frames < target_frames + largest_packet_frames + period_source_frames {
                return Err(ConfigurationError::new(format!(
                    "--max-buffer={milliseconds} ms has no jitter headroom: need at least {:.3} ms",
                    frames_to_milliseconds(
                        target_frames + largest_packet_frames + period_source_frames,
                        source_rate,
                    ),
                ))
                .into());
            }
            frames
        } else {
            (2 * target_frames).max(
                target_frames
                    .saturating_add(largest_packet_frames)
                    .saturating_add(period_source_frames),
            )
        };

        validate_derived_range(
            "--max-buffer",
            maximum_frames,
            source_rate,
            MAXIMUM_RANGE_MS,
        )?;

        let maximum_duration = frames_to_duration(maximum_frames, source_rate);
        let write_allowance = WRITE_ATTEMPT * WRITE_EXPIRY_ATTEMPTS;
        let maximum_age =
            maximum_duration.mul_f64(MAX_CORRECTION) + write_allowance + MAX_GROUP_DELAY;
        let maximum_age_periods =
            (maximum_age.as_nanos() / actual_period.as_nanos()).min(u64::MAX as u128) as u64;

        Ok(BufferLimits {
            target_frames,
            maximum_frames,
            largest_packet_frames,
            maximum_age_periods,
            submission_bound: maximum_age + actual_period,
        })
    }

    pub fn requested_alsa_buffer(&self) -> Duration {
        Duration::from_millis(self.alsa_buffer_ms as u64)
    }

    pub fn starvation_reconnect(&self) -> Duration {
        Duration::from_millis(self.starvation_reconnect_ms as u64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferLimits {
    pub target_frames: usize,
    pub maximum_frames: usize,
    pub largest_packet_frames: usize,
    pub maximum_age_periods: u64,
    pub submission_bound: Duration,
}

pub fn milliseconds_to_frames(milliseconds: u32, rate: u32) -> usize {
    (milliseconds as u64 * rate as u64).div_ceil(1000) as usize
}

pub fn duration_to_frames_ceil(duration: Duration, rate: u32) -> usize {
    let numerator = duration.as_nanos().saturating_mul(rate as u128);
    numerator.div_ceil(1_000_000_000).min(usize::MAX as u128) as usize
}

pub fn frames_to_duration(frames: usize, rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / rate as f64)
}

fn frames_to_milliseconds(frames: usize, rate: u32) -> f64 {
    frames as f64 * 1000.0 / rate as f64
}

fn validate_derived_range(
    name: &str,
    frames: usize,
    rate: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<()> {
    let milliseconds = frames_to_milliseconds(frames, rate);
    let minimum_frames = milliseconds_to_frames(*range.start(), rate);
    let maximum_frames = milliseconds_to_frames(*range.end(), rate);
    if !(minimum_frames..=maximum_frames).contains(&frames) {
        return Err(ConfigurationError::new(format!(
            "derived {name}={milliseconds:.3} ms is outside {}..={} ms",
            range.start(),
            range.end(),
        ))
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PlayPolicy, REQUESTED_PERIOD};
    use crate::error::ConfigurationError;

    fn policy(target: Option<u32>, maximum: Option<u32>) -> PlayPolicy {
        PlayPolicy {
            target_buffer_ms: target,
            default_target_buffer_ms: 50,
            maximum_buffer_ms: maximum,
            alsa_buffer_ms: 40,
            starvation_reconnect_ms: 1500,
        }
    }

    #[test]
    fn derives_usb_he_aac_headroom_before_first_insert() {
        let limits = policy(None, None)
            .derive_limits(44_100, 2048, REQUESTED_PERIOD)
            .unwrap();

        assert_eq!(limits.target_frames, 2048 + 441);
        assert_eq!(limits.maximum_frames, 2 * (2048 + 441));
    }

    #[test]
    fn explicit_target_requires_packet_and_period_headroom() {
        let error = policy(Some(50), None)
            .derive_limits(44_100, 2048, REQUESTED_PERIOD)
            .unwrap_err();

        assert!(error.is::<ConfigurationError>());
        assert!(error.to_string().contains("packet headroom"));
    }

    #[test]
    fn explicit_maximum_requires_jitter_headroom() {
        let error = policy(Some(100), Some(150))
            .derive_limits(44_100, 2048, REQUESTED_PERIOD)
            .unwrap_err();

        assert!(error.is::<ConfigurationError>());
        assert!(error.to_string().contains("jitter headroom"));
    }

    #[test]
    fn derived_values_are_checked_against_cli_ranges() {
        let error = policy(None, None)
            .derive_limits(8_000, 4096, REQUESTED_PERIOD)
            .unwrap_err();

        assert!(error.is::<ConfigurationError>());
        assert!(error.to_string().contains("outside"));
    }

    #[test]
    fn upper_cli_bound_allows_one_frame_of_duration_quantization() {
        let limits = policy(Some(500), Some(2000))
            .derive_limits(11_025, 1024, REQUESTED_PERIOD)
            .unwrap();
        assert_eq!(limits.target_frames, 5513);
    }
}

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, PolynomialDegree, Resampler};

const MIN_CORRECTION: f64 = 0.98;
const MAX_CORRECTION: f64 = 1.02;
const DAMPING: f64 = std::f64::consts::FRAC_1_SQRT_2;

pub struct FixedOutputResampler {
    resampler: Async<f32>,
    output_frames: usize,
    output: Vec<f32>,
}

impl FixedOutputResampler {
    pub fn new(source_rate: u32, output_rate: u32, output_frames: usize) -> Result<Self> {
        ensure!(source_rate > 0, "source sample rate must be positive");
        ensure!(output_rate > 0, "output sample rate must be positive");
        ensure!(output_frames > 0, "output period must contain frames");

        let nominal_ratio = output_rate as f64 / source_rate as f64;
        let relative_headroom = MAX_CORRECTION.max(1.0 / MIN_CORRECTION);
        let resampler = Async::<f32>::new_poly(
            nominal_ratio,
            relative_headroom,
            PolynomialDegree::Cubic,
            output_frames,
            1,
            FixedAsync::Output,
        )
        .map_err(|error| anyhow::anyhow!(error))
        .context("failed to create fixed-output resampler")?;

        Ok(Self {
            resampler,
            output_frames,
            output: vec![0.0; output_frames],
        })
    }

    pub fn input_frames_next(&self) -> usize {
        self.resampler.input_frames_next()
    }

    pub fn output_delay(&self, output_rate: u32) -> Duration {
        Duration::from_secs_f64(self.resampler.output_delay() as f64 / output_rate as f64)
    }

    #[cfg(test)]
    fn nominal_ratio(&self) -> f64 {
        self.resampler.resample_ratio()
    }

    pub fn set_correction(&mut self, correction: f64) -> Result<()> {
        ensure!(
            (MIN_CORRECTION..=MAX_CORRECTION).contains(&correction),
            "resampling correction {correction} is outside {MIN_CORRECTION}..={MAX_CORRECTION}"
        );
        self.resampler
            .set_resample_ratio_relative(correction, true)
            .map_err(|error| anyhow::anyhow!(error))
            .context("failed to set relative resampling correction")
    }

    pub fn process(&mut self, input: &[f32]) -> Result<&[f32]> {
        let required = self.input_frames_next();
        ensure!(
            input.len() == required,
            "fixed-output resampler needs {required} input frames, got {}",
            input.len()
        );

        let input_adapter =
            InterleavedSlice::new(input, 1, input.len()).expect("mono input has one valid channel");
        let mut output_adapter = InterleavedSlice::new_mut(&mut self.output, 1, self.output_frames)
            .expect("mono output has one valid channel");
        let (consumed, written) = self
            .resampler
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
            .map_err(|error| anyhow::anyhow!(error))
            .context("failed to resample fixed output period")?;
        ensure!(consumed == input.len(), "resampler left input unconsumed");
        ensure!(
            written == self.output_frames,
            "resampler wrote {written} frames instead of {}",
            self.output_frames
        );
        Ok(&self.output)
    }

    pub fn reset(&mut self) {
        self.resampler.reset();
        self.output.fill(0.0);
    }
}

#[derive(Clone, Debug)]
pub struct OccupancyController {
    source_rate: u32,
    period_seconds: f64,
    target_seconds: f64,
    tau_seconds: f64,
    correction_tau_seconds: Option<f64>,
    kp: f64,
    ki: f64,
    integral_limit: f64,
    integral: f64,
    filtered_seconds: Option<f64>,
    correction: f64,
    saturation_seconds: f64,
    saturation_direction: i8,
}

impl OccupancyController {
    pub fn new(
        source_rate: u32,
        target_frames: usize,
        largest_packet_frames: usize,
        period: Duration,
    ) -> Self {
        let period_seconds = period.as_secs_f64();
        let target_seconds = target_frames as f64 / source_rate as f64;
        let (tau_seconds, correction_tau_seconds, kp, ki) =
            gains(source_rate, largest_packet_frames);

        Self {
            source_rate,
            period_seconds,
            target_seconds,
            tau_seconds,
            correction_tau_seconds,
            kp,
            ki,
            integral_limit: 0.015 / ki,
            integral: 0.0,
            filtered_seconds: None,
            correction: 1.0,
            saturation_seconds: 0.0,
            saturation_direction: 0,
        }
    }

    pub fn update(&mut self, occupancy_frames: usize) -> f64 {
        let occupancy_seconds = occupancy_frames as f64 / self.source_rate as f64;
        let filtered = self.filtered_seconds.get_or_insert(occupancy_seconds);
        let alpha = 1.0 - (-self.period_seconds / self.tau_seconds).exp();
        *filtered += alpha * (occupancy_seconds - *filtered);
        let error = *filtered - self.target_seconds;

        let proposed = (self.integral + error * self.period_seconds)
            .clamp(-self.integral_limit, self.integral_limit);
        let unclamped = 1.0 - self.kp * error - self.ki * proposed;
        let drives_further_into_saturation = (unclamped < MIN_CORRECTION && error > 0.0)
            || (unclamped > MAX_CORRECTION && error < 0.0);
        if !drives_further_into_saturation {
            self.integral = proposed;
        }

        let command =
            (1.0 - self.kp * error - self.ki * self.integral).clamp(MIN_CORRECTION, MAX_CORRECTION);
        let saturation_direction = if unclamped < MIN_CORRECTION {
            -1
        } else if unclamped > MAX_CORRECTION {
            1
        } else {
            0
        };
        if saturation_direction != 0 && saturation_direction == self.saturation_direction {
            self.saturation_seconds += self.period_seconds;
        } else if saturation_direction != 0 {
            self.saturation_seconds = self.period_seconds;
        } else {
            self.saturation_seconds = 0.0;
        }
        self.saturation_direction = saturation_direction;
        if let Some(tau) = self.correction_tau_seconds {
            let alpha = 1.0 - (-self.period_seconds / tau).exp();
            self.correction += alpha * (command - self.correction);
        } else {
            self.correction = command;
        }
        self.correction = self.correction.clamp(MIN_CORRECTION, MAX_CORRECTION);
        self.correction
    }

    pub fn reset_filter(&mut self, occupancy_frames: usize) {
        self.filtered_seconds = Some(occupancy_frames as f64 / self.source_rate as f64);
    }

    pub fn reset_all(&mut self, occupancy_frames: usize) {
        self.integral = 0.0;
        self.correction = 1.0;
        self.saturation_seconds = 0.0;
        self.saturation_direction = 0;
        self.reset_filter(occupancy_frames);
    }

    pub fn reconfigure(&mut self, target_frames: usize, largest_packet_frames: usize) {
        let old_error = self.filtered_error_seconds();
        let old_command = self.kp * old_error + self.ki * self.integral;
        self.target_seconds = target_frames as f64 / self.source_rate as f64;
        let (tau_seconds, correction_tau_seconds, kp, ki) =
            gains(self.source_rate, largest_packet_frames);
        self.tau_seconds = tau_seconds;
        self.correction_tau_seconds = correction_tau_seconds;
        self.kp = kp;
        self.ki = ki;
        self.integral_limit = 0.015 / ki;
        let new_error = self.filtered_error_seconds();
        self.integral =
            ((old_command - kp * new_error) / ki).clamp(-self.integral_limit, self.integral_limit);
    }

    pub fn correction(&self) -> f64 {
        self.correction
    }

    pub fn filtered_error_seconds(&self) -> f64 {
        self.filtered_seconds.unwrap_or(self.target_seconds) - self.target_seconds
    }

    pub fn saturation_duration(&self) -> Duration {
        Duration::from_secs_f64(self.saturation_seconds)
    }

    #[cfg(test)]
    fn gains(&self) -> (f64, f64) {
        (self.kp, self.ki)
    }

    #[cfg(test)]
    fn integral(&self) -> f64 {
        self.integral
    }
}

fn gains(source_rate: u32, largest_packet_frames: usize) -> (f64, Option<f64>, f64, f64) {
    let packet_seconds = largest_packet_frames as f64 / source_rate as f64;
    let tau_seconds = 0.100_f64.max(2.0 * packet_seconds);
    let packet_rate = 1.0 / packet_seconds;
    let low_packet_rate = packet_rate < 20.0;
    let separation = if low_packet_rate { 40.0 } else { 7.0 };
    let omega_n = std::f64::consts::SQRT_2.min(1.0 / (separation * tau_seconds));
    let kp = 2.0 * DAMPING * omega_n;
    let ki = omega_n * omega_n;
    let correction_tau_seconds = Some(1.1 * packet_seconds);
    (tau_seconds, correction_tau_seconds, kp, ki)
}

#[cfg(test)]
mod tests {
    use super::{FixedOutputResampler, OccupancyController};
    use crate::policy::MAX_GROUP_DELAY;
    use std::time::Duration;

    const ACCEPTED_RATES: [u32; 12] = [
        96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
        8_000,
    ];

    #[test]
    fn fixed_output_uses_nominal_rate_ratio_and_exact_period() {
        let mut resampler = FixedOutputResampler::new(44_100, 48_000, 480).unwrap();
        assert!((resampler.nominal_ratio() - 48_000.0 / 44_100.0).abs() < 1e-12);

        let input = vec![0.25; resampler.input_frames_next()];
        assert_eq!(resampler.process(&input).unwrap().len(), 480);
    }

    #[test]
    fn cubic_group_delay_is_within_the_design_gate() {
        let mut maximum = Duration::ZERO;
        for source_rate in ACCEPTED_RATES {
            for output_rate in ACCEPTED_RATES {
                let frames = (output_rate / 100).max(1) as usize;
                let resampler =
                    FixedOutputResampler::new(source_rate, output_rate, frames).unwrap();
                assert!(
                    resampler.output_delay(output_rate) <= Duration::from_millis(1),
                    "{source_rate} -> {output_rate} delay {:?}",
                    resampler.output_delay(output_rate)
                );
                maximum = maximum.max(resampler.output_delay(output_rate));
            }
        }
        assert_eq!(maximum, MAX_GROUP_DELAY);
    }

    #[test]
    fn common_controller_has_the_derived_gains() {
        let controller = OccupancyController::new(44_100, 4_410, 1024, Duration::from_millis(10));
        let (kp, ki) = controller.gains();
        assert!((kp - 2.0).abs() < 0.001);
        assert!((ki - 2.0).abs() < 0.001);
    }

    #[test]
    fn slow_packet_rate_gets_the_aggressive_bandwidth_cap() {
        let controller = OccupancyController::new(8_000, 2_128, 2048, Duration::from_millis(10));
        let (kp, ki) = controller.gains();
        let omega_n = 1.0 / (40.0 * 0.512);
        assert!((kp - 2.0 * std::f64::consts::FRAC_1_SQRT_2 * omega_n).abs() < 1e-9);
        assert!((ki - omega_n * omega_n).abs() < 1e-9);
    }

    #[test]
    fn splice_filter_reset_preserves_integral() {
        let mut controller =
            OccupancyController::new(44_100, 4_410, 1024, Duration::from_millis(10));
        for _ in 0..100 {
            controller.update(4_851);
        }
        let integral = controller.integral();
        controller.reset_filter(4_410);
        assert_eq!(controller.integral(), integral);
        controller.reset_all(4_410);
        assert_eq!(controller.integral(), 0.0);
    }

    #[test]
    fn reconfigure_is_bumpless_and_preserves_pi_command() {
        let mut controller =
            OccupancyController::new(44_100, 4_410, 1024, Duration::from_millis(10));
        for _ in 0..100 {
            controller.update(4_851);
        }
        let before = controller.correction();
        controller.reconfigure(4_410, 2048);
        assert!((controller.correction() - before).abs() < 1e-12);
    }

    #[test]
    fn saturation_duration_tracks_only_continuous_clamping() {
        let mut controller =
            OccupancyController::new(44_100, 4_410, 1024, Duration::from_millis(10));
        controller.reset_filter(44_100);
        for _ in 0..500 {
            controller.update(44_100);
        }
        assert!(controller.saturation_duration() >= Duration::from_secs(5));
        assert_eq!(controller.integral(), 0.0);

        controller.reset_filter(0);
        controller.update(0);
        assert!(controller.saturation_duration() <= Duration::from_millis(10));

        controller.reset_all(4_410);
        controller.update(4_410);
        assert_eq!(controller.saturation_duration(), Duration::ZERO);
    }

    #[test]
    fn correction_ripple_stays_within_frequency_weighted_limits() {
        for source_rate in ACCEPTED_RATES {
            for packet_frames in [1024, 2048] {
                for period_ms in [9, 10, 11] {
                    let trace = simulate(source_rate, packet_frames, period_ms, 0.0, 120.0);
                    let steady = &trace[trace.len() / 2..];
                    let minimum = steady
                        .iter()
                        .map(|point| point.correction)
                        .fold(f64::INFINITY, f64::min);
                    let maximum = steady
                        .iter()
                        .map(|point| point.correction)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let ripple_points = (maximum - minimum) * 100.0;
                    let packet_rate = source_rate as f64 / packet_frames as f64;
                    let limit = if packet_rate < 20.0 { 0.12 } else { 1.8 };
                    let margin_limit = limit / 1.2;
                    assert!(
                        ripple_points <= margin_limit,
                        "rate={source_rate} packet={packet_frames} period={period_ms}ms ripple={ripple_points:.4}pp margin_limit={margin_limit:.4}pp ceiling={limit:.2}pp"
                    );
                    if source_rate == 44_100 && packet_frames == 1024 && period_ms == 10 {
                        assert!(
                            ripple_points <= 0.8 / 1.2,
                            "common-case ripple={ripple_points:.4}pp exceeds 20%-margin limit"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn one_percent_sender_step_meets_the_occupancy_band_after_five_seconds() {
        for source_rate in ACCEPTED_RATES {
            for packet_frames in [1024, 2048] {
                for period_ms in [9, 10, 11] {
                    let trace = simulate(source_rate, packet_frames, period_ms, 0.01, 40.0);
                    let period_seconds = period_ms as f64 / 1000.0;
                    let after_five = &trace[(5.0 / period_seconds) as usize..];
                    let band_frames = packet_frames as f64 + source_rate as f64 * period_seconds;
                    assert!(
                        after_five.iter().all(|point| {
                            point.occupancy_error_frames.abs() <= band_frames + 1.0
                                && (0.98..=1.02).contains(&point.correction)
                        }),
                        "rate={source_rate} packet={packet_frames} period={period_ms}ms exceeded band={band_frames:.1} frames"
                    );
                }
            }
        }
    }

    #[test]
    fn largest_admitted_filter_time_constant_meets_ripple_and_step_bounds() {
        let source_rate = 8_000;
        let packet_frames = 3_920;
        let period_ms = 10;
        let zero_drift = simulate(source_rate, packet_frames, period_ms, 0.0, 120.0);
        let steady = &zero_drift[zero_drift.len() / 2..];
        let minimum = steady
            .iter()
            .map(|point| point.correction)
            .fold(f64::INFINITY, f64::min);
        let maximum = steady
            .iter()
            .map(|point| point.correction)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!((maximum - minimum) * 100.0 <= 0.12 / 1.2);

        let step = simulate(source_rate, packet_frames, period_ms, 0.01, 40.0);
        let band_frames = packet_frames as f64 + 80.0;
        assert!(step[500..].iter().all(|point| {
            point.occupancy_error_frames.abs() <= band_frames + 1.0
                && (0.98..=1.02).contains(&point.correction)
        }));
    }

    #[derive(Clone, Copy)]
    struct SimulationPoint {
        correction: f64,
        occupancy_error_frames: f64,
    }

    fn simulate(
        source_rate: u32,
        packet_frames: usize,
        period_ms: u64,
        sender_error: f64,
        seconds: f64,
    ) -> Vec<SimulationPoint> {
        let period = Duration::from_millis(period_ms);
        let period_seconds = period.as_secs_f64();
        let period_frames = source_rate as f64 * period_seconds;
        let target_frames = (source_rate as usize / 10)
            .max(packet_frames + (source_rate as usize * period_ms as usize).div_ceil(1000));
        let mut controller =
            OccupancyController::new(source_rate, target_frames, packet_frames, period);
        controller.reset_filter(target_frames);
        let steps = (seconds / period_seconds) as usize;
        let warmup_steps = (1000.0 / period_seconds) as usize;
        let mut occupancy = target_frames as f64;
        let mut arrival_accumulator = 0.0;
        let mut trace = Vec::with_capacity(steps);

        for step in 0..warmup_steps + steps {
            let drift = if step < warmup_steps {
                0.0
            } else {
                sender_error
            };
            arrival_accumulator += period_frames * (1.0 + drift);
            while arrival_accumulator >= packet_frames as f64 {
                occupancy += packet_frames as f64;
                arrival_accumulator -= packet_frames as f64;
            }
            let correction = controller.update(occupancy.max(0.0).round() as usize);
            occupancy -= period_frames / correction;
            if step >= warmup_steps {
                trace.push(SimulationPoint {
                    correction,
                    occupancy_error_frames: occupancy - target_frames as f64,
                });
            }
        }
        trace
    }
}

use anyhow::{Context, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Adjustable, Async, FixedAsync, PolynomialDegree, Resampler, Resizable};

pub struct AdaptiveResampler {
    resampler: Async<f32>,
    max_input_frames: usize,
    work_buf: Vec<f32>,
    out_buf: Vec<f32>,
    target_delay: f64,
    kp: f64,
    ki: f64,
    integral: f64,
}

impl AdaptiveResampler {
    pub fn new(chunk_size: usize, target_delay_samples: u32) -> Result<Self> {
        let resampler = Async::<f32>::new_poly(
            1.0,
            1.1,
            PolynomialDegree::Cubic,
            chunk_size,
            1,
            FixedAsync::Input,
        )
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("Failed to create resampler")?;

        let work_buf = vec![0.0f32; resampler.output_frames_max()];
        let out_buf_capacity = work_buf.len();

        Ok(Self {
            resampler,
            max_input_frames: chunk_size,
            work_buf,
            out_buf: Vec::with_capacity(out_buf_capacity),
            target_delay: target_delay_samples as f64,
            kp: 1e-5,
            ki: 1e-8,
            integral: 0.0,
        })
    }

    pub fn process(&mut self, input: &[f32], alsa_delay: i64) -> Result<&[f32]> {
        let error = alsa_delay as f64 - self.target_delay;
        self.integral = (self.integral + error).clamp(-1e6, 1e6);
        let ratio = (1.0 - self.kp * error - self.ki * self.integral).clamp(1.0 / 1.1, 1.1);
        self.resampler
            .set_resample_ratio(ratio, true)
            .map_err(|e| anyhow::anyhow!(e))
            .context("failed to set resampling ratio")?;

        if std::env::var_os("IOSMIC_RESAMPLE_DEBUG").is_some() {
            eprintln!(
                "resample delay={} target={:.0} error={:.1} integral={:.0} ratio={:.6}",
                alsa_delay, self.target_delay, error, self.integral, ratio
            );
        }

        self.out_buf.clear();
        let work_buf_frames = self.work_buf.len();
        for chunk in input.chunks(self.max_input_frames) {
            self.resampler
                .set_chunk_size(chunk.len())
                .map_err(|e| anyhow::anyhow!(e))
                .context("failed to set resampling chunk size")?;

            let input_adapter = InterleavedSlice::new(chunk, 1, chunk.len())
                .expect("input chunk has one valid channel");
            let mut output_adapter =
                InterleavedSlice::new_mut(&mut self.work_buf, 1, work_buf_frames)
                    .expect("resampling work buffer has one valid channel");
            let (consumed, written) = self
                .resampler
                .process_into_buffer(&input_adapter, &mut output_adapter, None)
                .map_err(|e| anyhow::anyhow!(e))
                .context("failed to resample audio")?;
            debug_assert_eq!(consumed, chunk.len());
            self.out_buf.extend_from_slice(&self.work_buf[..written]);
        }

        Ok(&self.out_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ratio_clamps_upper_when_delay_below_target() {
        // error = 0 - 44100 = -44100, kp*error = -0.441, ratio = 1.441 -> clamped to 1.1
        let mut r = AdaptiveResampler::new(1024, 44100).unwrap();
        let input = vec![0.0f32; 1024];
        let out = r.process(&input, 0).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn ratio_clamps_lower_when_delay_above_target() {
        // error = 200000 - 0 = 200000, kp*error = 2.0, ratio = -1.0 -> clamped to 1/1.1 ≈ 0.909
        let mut r = AdaptiveResampler::new(1024, 0).unwrap();
        let input = vec![0.0f32; 1024];
        let out = r.process(&input, 200000).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn process_returns_non_empty_for_nominal_chunk() {
        // error = 2205 - 2205 = 0, ratio = 1.0
        let mut r = AdaptiveResampler::new(1024, 2205).unwrap();
        let input = vec![0.0f32; 1024];
        let out = r.process(&input, 2205).unwrap();
        assert!(!out.is_empty());
    }

    #[test]
    fn process_short_chunk_without_zero_padding() {
        let mut r = AdaptiveResampler::new(1024, 2205).unwrap();
        let input = vec![0.25f32; 960];
        let out = r.process(&input, 2205).unwrap();
        assert!(
            out.len() < 1024,
            "short input must not be padded to a full block"
        );
        assert!(
            out.len() >= 950,
            "resampler should consume the whole short input"
        );
    }

    #[test]
    fn process_consumes_an_oversized_chunk_immediately() {
        let mut r = AdaptiveResampler::new(1024, 2205).unwrap();
        let input = vec![0.25f32; 2048];
        let out = r.process(&input, 2205).unwrap();
        assert!(out.len() > 1900, "oversized input must not lose its tail");
    }

    proptest! {
        #[test]
        fn process_never_panics_for_any_delay(delay in (-1_000_000_000i64..=1_000_000_000i64)) {
            let mut r = AdaptiveResampler::new(1024, 2205).unwrap();
            let input = vec![0.0f32; 1024];
            let _out = r.process(&input, delay);
        }
    }
}

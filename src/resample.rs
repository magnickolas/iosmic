use anyhow::{Context, Result};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler};

pub struct AdaptiveResampler {
    resampler: Async<f32>,
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

        let out_buf = vec![0.0f32; resampler.output_frames_max()];

        Ok(Self {
            resampler,
            out_buf,
            target_delay: target_delay_samples as f64,
            kp: 1e-5,
            ki: 1e-8,
            integral: 0.0,
        })
    }

    pub fn process(&mut self, input: &[f32], alsa_delay: i64) -> &[f32] {
        let error = alsa_delay as f64 - self.target_delay;
        self.integral = (self.integral + error).clamp(-1e6, 1e6);
        let ratio = (1.0 - self.kp * error - self.ki * self.integral).clamp(1.0 / 1.1, 1.1);
        let _ = self.resampler.set_resample_ratio(ratio, true);

        if std::env::var_os("IOSMIC_RESAMPLE_DEBUG").is_some() {
            eprintln!(
                "resample delay={} target={:.0} error={:.1} integral={:.0} ratio={:.6}",
                alsa_delay, self.target_delay, error, self.integral, ratio
            );
        }

        let input_adapter = InterleavedSlice::new(input, 1, input.len()).unwrap();
        let out_frames = self.out_buf.len();
        let mut output_adapter =
            InterleavedSlice::new_mut(&mut self.out_buf, 1, out_frames).unwrap();

        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: Some(input.len()),
            active_channels_mask: None,
        };

        match self.resampler.process_into_buffer(
            &input_adapter,
            &mut output_adapter,
            Some(&indexing),
        ) {
            Ok((_, written)) => &self.out_buf[..written],
            Err(_) => &[],
        }
    }
}

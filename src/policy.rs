use anyhow::Result;

use crate::resample::AdaptiveResampler;
use crate::sink::AudioSink;

#[derive(Clone)]
pub struct PlayPolicy {
    pub buffer_ms: u32,
    pub latency_reconnect_ms: u32,
}

impl PlayPolicy {
    pub fn buffer_us(&self) -> u32 {
        self.buffer_ms * 1000
    }
}

pub struct PolicyState<S> {
    sink: S,
    prefill_buf: Vec<f32>,
    prefilled: bool,
    buffer_samples: u32,
    latency_reconnect_samples: i64,
    resampler: AdaptiveResampler,
}

impl<S: AudioSink> PolicyState<S> {
    pub fn new(policy: &PlayPolicy, sample_rate: u32, chunk_size: usize, sink: S) -> Result<Self> {
        let buffer_samples = (sample_rate as u64 * policy.buffer_ms as u64 / 1000) as u32;
        let latency_reconnect_samples =
            (sample_rate as u64 * policy.latency_reconnect_ms as u64 / 1000) as i64;
        let resampler = AdaptiveResampler::new(chunk_size, buffer_samples)?;
        Ok(Self {
            sink,
            prefill_buf: Vec::new(),
            prefilled: false,
            buffer_samples,
            latency_reconnect_samples,
            resampler,
        })
    }

    /// Write samples through the playback pipeline. Returns the current delay in samples.
    pub fn write(&mut self, samples: &[f32]) -> Result<i64> {
        if !self.prefilled {
            self.prefill_buf.extend_from_slice(samples);
            if self.prefill_buf.len() >= self.buffer_samples as usize {
                let s32: Vec<i32> = self.prefill_buf.iter().map(|&s| f32_to_s32(s)).collect();
                self.sink.write(&s32)?;
                self.prefill_buf = Vec::new();
                self.prefilled = true;
            }
            return Ok(self.sink.delay_samples());
        }

        let delay = self.sink.delay_samples();
        if self.latency_reconnect_samples > 0 && delay > self.latency_reconnect_samples {
            anyhow::bail!("Latency {delay} samples exceeds reconnect threshold");
        }
        let resampled = self.resampler.process(samples, delay);
        let s32: Vec<i32> = resampled.iter().map(|&s| f32_to_s32(s)).collect();
        self.sink.write(&s32)?;
        Ok(delay)
    }
}

fn f32_to_s32(s: f32) -> i32 {
    (s * i32::MAX as f32) as i32
}

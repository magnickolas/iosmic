use alsa::pcm::PCM;
use anyhow::{Context, Result};

use crate::resample::AdaptiveResampler;

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

pub struct PolicyState {
    prefill_buf: Vec<f32>,
    prefilled: bool,
    buffer_samples: u32,
    latency_reconnect_samples: i64,
    resampler: AdaptiveResampler,
}

impl PolicyState {
    pub fn new(policy: &PlayPolicy, sample_rate: u32, chunk_size: usize) -> Result<Self> {
        let buffer_samples = (sample_rate as u64 * policy.buffer_ms as u64 / 1000) as u32;
        let latency_reconnect_samples =
            (sample_rate as u64 * policy.latency_reconnect_ms as u64 / 1000) as i64;
        let resampler = AdaptiveResampler::new(chunk_size, buffer_samples)?;
        Ok(Self {
            prefill_buf: Vec::new(),
            prefilled: false,
            buffer_samples,
            latency_reconnect_samples,
            resampler,
        })
    }

    pub fn write(&mut self, pcm: &PCM, samples: &[f32]) -> Result<()> {
        if !self.prefilled {
            self.prefill_buf.extend_from_slice(samples);
            if self.prefill_buf.len() >= self.buffer_samples as usize {
                let s32: Vec<i32> = self.prefill_buf.iter().map(|&s| f32_to_s32(s)).collect();
                write_pcm(pcm, &s32)?;
                self.prefill_buf = Vec::new();
                self.prefilled = true;
            }
        } else {
            let delay = delay_samples(pcm);
            if self.latency_reconnect_samples > 0 && delay > self.latency_reconnect_samples {
                anyhow::bail!("Latency {delay} samples exceeds reconnect threshold");
            }
            let resampled = self.resampler.process(samples, delay);
            let s32: Vec<i32> = resampled.iter().map(|&s| f32_to_s32(s)).collect();
            write_pcm(pcm, &s32)?;
        }
        Ok(())
    }
}

fn f32_to_s32(s: f32) -> i32 {
    (s * i32::MAX as f32) as i32
}

pub fn delay_samples(pcm: &PCM) -> i64 {
    pcm.delay().unwrap_or(0)
}

fn write_pcm(pcm: &PCM, data: &[i32]) -> Result<()> {
    let io = pcm.io_i32().context("Failed to create PCM i32 writer")?;
    let mut offset = 0;

    while offset < data.len() {
        match io.writei(&data[offset..]) {
            Ok(0) => anyhow::bail!("PCM write made no progress"),
            Ok(written) => offset += written,
            Err(e) if e.errno() == libc::EPIPE => {
                pcm.prepare().context("PCM prepare after underrun failed")?;
            }
            Err(e) => return Err(e).context("PCM write failed"),
        }
    }

    Ok(())
}

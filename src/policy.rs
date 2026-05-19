use alsa::pcm::PCM;
use anyhow::{Context, Result};

#[derive(Clone)]
pub struct PlayPolicy {
    pub buffer_ms: u32,
}

impl PlayPolicy {
    pub fn buffer_us(&self) -> u32 {
        self.buffer_ms * 1000
    }
}

pub struct PolicyState {
    prefill_buf: Vec<i32>,
    prefilled: bool,
    buffer_samples: u32,
}

impl PolicyState {
    pub fn new(policy: &PlayPolicy, sample_rate: u32) -> Self {
        let buffer_samples = (sample_rate as u64 * policy.buffer_ms as u64 / 1000) as u32;
        Self {
            prefill_buf: Vec::new(),
            prefilled: false,
            buffer_samples,
        }
    }

    pub fn write(&mut self, pcm: &PCM, samples: &[i32]) -> Result<()> {
        if !self.prefilled {
            self.prefill_buf.extend_from_slice(samples);
            if self.prefill_buf.len() >= self.buffer_samples as usize {
                write_pcm(pcm, &self.prefill_buf)?;
                self.prefill_buf = Vec::new();
                self.prefilled = true;
            }
        } else {
            write_pcm(pcm, samples)?;
        }
        Ok(())
    }
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

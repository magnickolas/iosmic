use alsa::pcm::PCM;
use anyhow::{Context, Result};

#[derive(Clone)]
pub enum PlayPolicy {
    Record { buffer_ms: u32 },
    Drop { max_delay_ms: u32 },
}

impl PlayPolicy {
    pub fn buffer_us(&self) -> u32 {
        match self {
            Self::Record { buffer_ms } => buffer_ms * 1000,
            Self::Drop { max_delay_ms } => max_delay_ms * 2 * 1000,
        }
    }
}

pub enum PolicyState {
    Record {
        prefill_buf: Vec<i32>,
        prefilled: bool,
        buffer_samples: u32,
    },
    Drop {
        max_delay_samples: i64,
    },
}

impl PolicyState {
    pub fn new(policy: &PlayPolicy, sample_rate: u32) -> Self {
        let ms_to_samples = |ms: u32| (sample_rate as u64 * ms as u64 / 1000) as u32;
        match policy {
            PlayPolicy::Record { buffer_ms } => Self::Record {
                prefill_buf: Vec::new(),
                prefilled: false,
                buffer_samples: ms_to_samples(*buffer_ms),
            },
            PlayPolicy::Drop { max_delay_ms } => Self::Drop {
                max_delay_samples: ms_to_samples(*max_delay_ms) as i64,
            },
        }
    }

    pub fn write(&mut self, pcm: &PCM, samples: &[i32]) -> Result<()> {
        match self {
            Self::Record {
                prefill_buf,
                prefilled,
                buffer_samples,
            } => {
                if !*prefilled {
                    prefill_buf.extend_from_slice(samples);
                    if prefill_buf.len() >= *buffer_samples as usize {
                        write_pcm(pcm, prefill_buf)?;
                        prefill_buf.clear();
                        *prefilled = true;
                    }
                } else {
                    write_pcm(pcm, samples)?;
                }
            }
            Self::Drop { max_delay_samples } => {
                let delay = pcm_delay(pcm);
                if delay < *max_delay_samples {
                    write_pcm(pcm, samples)?;
                }
            }
        }

        Ok(())
    }
}

pub fn delay_samples(pcm: &PCM) -> i64 {
    pcm_delay(pcm)
}

fn pcm_delay(pcm: &PCM) -> i64 {
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

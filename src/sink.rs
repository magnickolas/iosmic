use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};

pub trait AudioSink {
    fn write(&mut self, samples: &[i32]) -> Result<()>;
    fn delay_samples(&self) -> i64;
}

impl AudioSink for Box<dyn AudioSink> {
    fn write(&mut self, samples: &[i32]) -> Result<()> {
        (**self).write(samples)
    }
    fn delay_samples(&self) -> i64 {
        (**self).delay_samples()
    }
}

pub struct AlsaSink {
    pcm: PCM,
}

impl AlsaSink {
    pub fn open(device: &str, rate: u32, buffer_us: u32) -> Result<Self> {
        let pcm =
            PCM::new(device, Direction::Playback, false).context("Failed to open ALSA device")?;

        {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_access(Access::RWInterleaved)?;
            hwp.set_format(Format::s32())?;
            hwp.set_channels(1)?;
            hwp.set_rate(rate, ValueOr::Nearest)?;
            hwp.set_buffer_time_near(buffer_us, ValueOr::Nearest)?;
            hwp.set_period_time_near(buffer_us / 4, ValueOr::Nearest)?;
            pcm.hw_params(&hwp)?;
        }

        pcm.prepare().context("PCM prepare failed")?;
        Ok(Self { pcm })
    }
}

impl AudioSink for AlsaSink {
    fn write(&mut self, data: &[i32]) -> Result<()> {
        let io = self
            .pcm
            .io_i32()
            .context("Failed to create PCM i32 writer")?;
        let mut offset = 0;

        while offset < data.len() {
            match io.writei(&data[offset..]) {
                Ok(0) => anyhow::bail!("PCM write made no progress"),
                Ok(written) => offset += written,
                Err(e) if e.errno() == libc::EPIPE => {
                    self.pcm
                        .prepare()
                        .context("PCM prepare after underrun failed")?;
                }
                Err(e) => return Err(e).context("PCM write failed"),
            }
        }

        Ok(())
    }

    fn delay_samples(&self) -> i64 {
        self.pcm.delay().unwrap_or(0)
    }
}

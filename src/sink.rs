use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use std::fmt;

#[derive(Debug)]
pub struct MissingPulsePlugin;

impl fmt::Display for MissingPulsePlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "ALSA's `pulse` PCM device is unavailable. Install the ALSA plugins package that provides the `pulse` plugin, then verify it with `aplay -L | grep -x pulse`. See README.md for package commands.",
        )
    }
}

impl std::error::Error for MissingPulsePlugin {}

#[cfg_attr(test, mockall::automock)]
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
        let pcm = match PCM::new(device, Direction::Playback, false) {
            Ok(pcm) => pcm,
            Err(error) if is_missing_pulse_plugin(device, error.errno()) => {
                return Err(MissingPulsePlugin.into());
            }
            Err(error) => return Err(error).context("Failed to open ALSA device"),
        };

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

fn is_missing_pulse_plugin(device: &str, errno: i32) -> bool {
    device == "pulse" && errno == libc::ENOENT
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

#[cfg(test)]
mod tests {
    use super::{AudioSink, MockAudioSink, is_missing_pulse_plugin};

    #[test]
    fn mock_sink_blanket_impl_delegates() {
        let mut mock = MockAudioSink::new();
        mock.expect_write().times(1).returning(|_| Ok(()));
        mock.expect_delay_samples().times(1).returning(|| 42);

        let mut boxed: Box<dyn AudioSink> = Box::new(mock);

        assert!(boxed.write(&[1, 2, 3]).is_ok());
        assert_eq!(boxed.delay_samples(), 42);
    }

    #[test]
    fn recognizes_a_missing_pulse_plugin() {
        assert!(is_missing_pulse_plugin("pulse", libc::ENOENT));
        assert!(!is_missing_pulse_plugin("default", libc::ENOENT));
        assert!(!is_missing_pulse_plugin("pulse", libc::EPIPE));
    }
}

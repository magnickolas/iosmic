use std::fmt;
use std::time::Duration;

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};

use crate::error::ConfigurationError;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkParameters {
    pub rate: u32,
    pub period_frames: usize,
    pub buffer_frames: usize,
}

impl SinkParameters {
    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(self.period_frames as f64 / self.rate as f64)
    }

    pub fn buffer(&self) -> Duration {
        Duration::from_secs_f64(self.buffer_frames as f64 / self.rate as f64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteResult {
    Written(usize),
    WouldBlock,
    Underrun,
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResult {
    Ready,
    TimedOut,
    Underrun,
    Suspended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Frames(usize),
    Underrun,
    Suspended,
}

#[cfg_attr(test, mockall::automock)]
pub trait AudioSink: Send {
    fn parameters(&self) -> SinkParameters;
    fn wait_ready(&self, timeout: Duration) -> Result<WaitResult>;
    fn available_frames(&self) -> Result<Availability>;
    fn write(&mut self, samples: &[i32]) -> Result<WriteResult>;
    fn prepare(&mut self) -> Result<()>;
    fn drop_and_prepare(&mut self) -> Result<()>;
    fn drop_queue(&mut self) -> Result<()>;
    fn delay_frames(&self) -> Result<i64>;
}

impl AudioSink for Box<dyn AudioSink> {
    fn parameters(&self) -> SinkParameters {
        (**self).parameters()
    }

    fn wait_ready(&self, timeout: Duration) -> Result<WaitResult> {
        (**self).wait_ready(timeout)
    }

    fn available_frames(&self) -> Result<Availability> {
        (**self).available_frames()
    }

    fn write(&mut self, samples: &[i32]) -> Result<WriteResult> {
        (**self).write(samples)
    }

    fn prepare(&mut self) -> Result<()> {
        (**self).prepare()
    }

    fn drop_and_prepare(&mut self) -> Result<()> {
        (**self).drop_and_prepare()
    }

    fn drop_queue(&mut self) -> Result<()> {
        (**self).drop_queue()
    }

    fn delay_frames(&self) -> Result<i64> {
        (**self).delay_frames()
    }
}

pub trait SinkFactory: Send + Sync {
    fn open(
        &self,
        requested_rate: u32,
        requested_buffer: Duration,
        requested_period: Duration,
    ) -> Result<Box<dyn AudioSink>>;
}

#[derive(Clone, Debug)]
pub struct AlsaSinkFactory {
    device: String,
}

impl AlsaSinkFactory {
    pub fn new(device: impl Into<String>) -> Self {
        Self {
            device: device.into(),
        }
    }
}

impl SinkFactory for AlsaSinkFactory {
    fn open(
        &self,
        requested_rate: u32,
        requested_buffer: Duration,
        requested_period: Duration,
    ) -> Result<Box<dyn AudioSink>> {
        Ok(Box::new(AlsaSink::open(
            &self.device,
            requested_rate,
            requested_buffer,
            requested_period,
        )?))
    }
}

pub struct AlsaSink {
    pcm: PCM,
    parameters: SinkParameters,
}

impl AlsaSink {
    pub fn open(
        device: &str,
        requested_rate: u32,
        requested_buffer: Duration,
        requested_period: Duration,
    ) -> Result<Self> {
        let pcm = match PCM::new(device, Direction::Playback, true) {
            Ok(pcm) => pcm,
            Err(error) if is_missing_pulse_plugin(device, error.errno()) => {
                return Err(MissingPulsePlugin.into());
            }
            Err(error) => return Err(error).context("failed to open ALSA device"),
        };

        let parameters = {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_access(Access::RWInterleaved)?;
            hwp.set_format(Format::s32())?;
            hwp.set_channels(1)?;
            hwp.set_rate_near(requested_rate, ValueOr::Nearest)?;
            hwp.set_period_time_near(duration_micros(requested_period), ValueOr::Nearest)?;
            hwp.set_buffer_time_near(duration_micros(requested_buffer), ValueOr::Nearest)?;

            let parameters = SinkParameters {
                rate: hwp.get_rate()?,
                period_frames: usize::try_from(hwp.get_period_size()?)
                    .context("negative ALSA period size")?,
                buffer_frames: usize::try_from(hwp.get_buffer_size()?)
                    .context("negative ALSA buffer size")?,
            };
            pcm.hw_params(&hwp)?;
            parameters
        };

        validate_parameters(parameters, requested_period, requested_buffer)?;

        {
            let swp = pcm.sw_params_current()?;
            swp.set_avail_min(parameters.period_frames as i64)?;
            swp.set_start_threshold(parameters.buffer_frames as i64)?;
            pcm.sw_params(&swp)?;
        }

        pcm.prepare().context("PCM prepare failed")?;
        Ok(Self { pcm, parameters })
    }
}

impl AudioSink for AlsaSink {
    fn parameters(&self) -> SinkParameters {
        self.parameters
    }

    fn wait_ready(&self, timeout: Duration) -> Result<WaitResult> {
        match self
            .pcm
            .wait(Some(timeout.as_millis().min(u32::MAX as u128) as u32))
        {
            Ok(true) => Ok(WaitResult::Ready),
            Ok(false) => Ok(WaitResult::TimedOut),
            Err(error) if error.errno() == libc::EPIPE => Ok(WaitResult::Underrun),
            Err(error) if error.errno() == libc::ESTRPIPE => Ok(WaitResult::Suspended),
            Err(error) => Err(error).context("ALSA readiness wait failed"),
        }
    }

    fn available_frames(&self) -> Result<Availability> {
        match self.pcm.avail_update() {
            Ok(frames) => Ok(Availability::Frames(
                usize::try_from(frames).context("ALSA reported negative available frames")?,
            )),
            Err(error) if error.errno() == libc::EPIPE => Ok(Availability::Underrun),
            Err(error) if error.errno() == libc::ESTRPIPE => Ok(Availability::Suspended),
            Err(error) => Err(error).context("ALSA avail update failed"),
        }
    }

    fn write(&mut self, samples: &[i32]) -> Result<WriteResult> {
        let io = self
            .pcm
            .io_i32()
            .context("failed to create PCM i32 writer")?;
        match io.writei(samples) {
            Ok(written) => Ok(WriteResult::Written(written)),
            Err(error) if error.errno() == libc::EAGAIN => Ok(WriteResult::WouldBlock),
            Err(error) if error.errno() == libc::EPIPE => Ok(WriteResult::Underrun),
            Err(error) if error.errno() == libc::ESTRPIPE => Ok(WriteResult::Suspended),
            Err(error) => Err(error).context("PCM write failed"),
        }
    }

    fn prepare(&mut self) -> Result<()> {
        self.pcm.prepare().context("PCM prepare failed")
    }

    fn drop_and_prepare(&mut self) -> Result<()> {
        self.pcm.drop().context("PCM drop failed")?;
        self.pcm.prepare().context("PCM prepare after drop failed")
    }

    fn drop_queue(&mut self) -> Result<()> {
        self.pcm.drop().context("PCM drop failed")
    }

    fn delay_frames(&self) -> Result<i64> {
        self.pcm.delay().context("ALSA delay query failed")
    }
}

pub fn validate_parameters(
    actual: SinkParameters,
    requested_period: Duration,
    requested_buffer: Duration,
) -> Result<()> {
    if actual.rate == 0 || actual.period_frames == 0 || actual.buffer_frames == 0 {
        return Err(ConfigurationError::new(format!(
            "ALSA negotiated invalid parameters: {actual:?}"
        ))
        .into());
    }

    let period = actual.period();
    let period_error = period.abs_diff(requested_period);
    if period_error.as_secs_f64() > requested_period.as_secs_f64() * 0.10 {
        return Err(ConfigurationError::new(format!(
            "ALSA period mismatch: requested {:.3} ms, actual {:.3} ms",
            requested_period.as_secs_f64() * 1000.0,
            period.as_secs_f64() * 1000.0,
        ))
        .into());
    }

    if actual.buffer() > requested_buffer + period {
        return Err(ConfigurationError::new(format!(
            "ALSA buffer mismatch: requested {:.3} ms, actual {:.3} ms (limit {:.3} ms)",
            requested_buffer.as_secs_f64() * 1000.0,
            actual.buffer().as_secs_f64() * 1000.0,
            (requested_buffer + period).as_secs_f64() * 1000.0,
        ))
        .into());
    }
    Ok(())
}

fn duration_micros(duration: Duration) -> u32 {
    duration.as_micros().min(u32::MAX as u128) as u32
}

fn is_missing_pulse_plugin(device: &str, errno: i32) -> bool {
    device == "pulse" && errno == libc::ENOENT
}

#[cfg(test)]
mod tests {
    use super::{
        AudioSink, MockAudioSink, SinkParameters, WriteResult, is_missing_pulse_plugin,
        validate_parameters,
    };
    use crate::error::ConfigurationError;
    use std::time::Duration;

    #[test]
    fn mock_sink_blanket_impl_delegates() {
        let parameters = SinkParameters {
            rate: 48_000,
            period_frames: 480,
            buffer_frames: 1920,
        };
        let mut mock = MockAudioSink::new();
        mock.expect_parameters().return_const(parameters);
        mock.expect_write()
            .times(1)
            .returning(|_| Ok(WriteResult::Written(3)));

        let mut boxed: Box<dyn AudioSink> = Box::new(mock);
        assert_eq!(boxed.parameters(), parameters);
        assert_eq!(boxed.write(&[1, 2, 3]).unwrap(), WriteResult::Written(3));
    }

    #[test]
    fn validates_negotiated_period_and_buffer() {
        let requested_period = Duration::from_millis(10);
        let requested_buffer = Duration::from_millis(40);
        assert!(
            validate_parameters(
                SinkParameters {
                    rate: 48_000,
                    period_frames: 480,
                    buffer_frames: 1920,
                },
                requested_period,
                requested_buffer,
            )
            .is_ok()
        );

        let error = validate_parameters(
            SinkParameters {
                rate: 48_000,
                period_frames: 320,
                buffer_frames: 960,
            },
            requested_period,
            requested_buffer,
        )
        .unwrap_err();
        assert!(error.is::<ConfigurationError>());
        assert!(error.to_string().contains("period mismatch"));
    }

    #[test]
    fn recognizes_a_missing_pulse_plugin() {
        assert!(is_missing_pulse_plugin("pulse", libc::ENOENT));
        assert!(!is_missing_pulse_plugin("default", libc::ENOENT));
        assert!(!is_missing_pulse_plugin("pulse", libc::EPIPE));
    }
}

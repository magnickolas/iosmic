use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use byteorder::{BigEndian, ByteOrder};
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::connection::Stream;
use crate::debug::TimingDebug;
use crate::decode::AacDecoder;
use crate::policy::{PlayPolicy, PolicyState, delay_samples};

pub(crate) const AUDIO_REQ: &[u8] = b"GET /v1/audio.2";
const NO_PTS: u64 = u64::MAX;

pub async fn run(mut stream: Stream, policy: PlayPolicy, sink_device: &str) -> Result<()> {
    stream
        .write_all(AUDIO_REQ)
        .await
        .context("Failed to send audio request")?;

    let mut decoder: Option<AacDecoder> = None;
    let mut pcm: Option<PCM> = None;
    let mut state: Option<PolicyState> = None;
    let mut timing_debug = TimingDebug::from_env();

    const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

    loop {
        let frame = match tokio::time::timeout(IDLE_TIMEOUT, read_frame(&mut stream)).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(e)) if is_eof(&e) => {
                eprintln!("Stream ended");
                return Ok(());
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                anyhow::bail!("No data received for {}s", IDLE_TIMEOUT.as_secs());
            }
        };
        let (pts, audio_data) = match frame {
            Frame::Config(config) => {
                decoder = Some(AacDecoder::new(&config)?);
                continue;
            }
            Frame::Audio { pts, data } => (pts, data),
        };

        if pcm.is_none() {
            let dec = decoder.as_ref().context("No decoder")?;
            let buffer_us = policy.buffer_us();
            let alsa_buffer_us = buffer_us;
            eprintln!(
                "input {}Hz {}ch, buffer {}ms",
                dec.sample_rate(),
                dec.channels(),
                buffer_us / 1000
            );
            pcm = Some(open_pcm(sink_device, dec.sample_rate(), alsa_buffer_us)?);
            state = Some(PolicyState::new(&policy, dec.sample_rate(), 1024)?);
        }

        let dec = decoder.as_mut().context("Got audio data before config")?;
        let samples = dec.decode(&audio_data, pts)?;
        if samples.is_empty() {
            continue;
        }

        let (Some(p), Some(st)) = (pcm.as_ref(), state.as_mut()) else {
            continue;
        };

        let out = downmix_mono(&samples, dec.channels());
        if !out.is_empty() {
            timing_debug.log(pts, out.len(), dec.sample_rate(), delay_samples(p));
            st.write(p, &out)?;
        }
    }
}

fn downmix_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Config(Vec<u8>),
    Audio { pts: u64, data: Vec<u8> },
}

pub(crate) async fn read_frame<R>(stream: &mut R) -> Result<Frame>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; 12];
    stream
        .read_exact(&mut header)
        .await
        .context("Failed to read header")?;

    parse_frame(stream, header).await
}

async fn parse_frame<R>(stream: &mut R, header: [u8; 12]) -> Result<Frame>
where
    R: AsyncRead + Unpin,
{
    let pts = BigEndian::read_u64(&header[..8]);
    let len = BigEndian::read_u32(&header[8..12]);

    if pts == NO_PTS {
        if len == u32::MAX {
            anyhow::bail!("Stop/error from app side");
        }
        anyhow::ensure!(
            len > 0 && len <= 1024,
            "Config packet size invalid: {}",
            len
        );
        let mut config = vec![0u8; len as usize];
        stream
            .read_exact(&mut config)
            .await
            .context("Failed to read config")?;
        return Ok(Frame::Config(config));
    }

    anyhow::ensure!(
        len > 0 && len <= 1024 * 1024,
        "Data packet size invalid: {}",
        len
    );
    let mut data = vec![0u8; len as usize];
    stream
        .read_exact(&mut data)
        .await
        .context("Failed to read audio data")?;

    Ok(Frame::Audio { pts, data })
}

fn open_pcm(device: &str, rate: u32, latency_us: u32) -> Result<PCM> {
    let pcm = PCM::new(device, Direction::Playback, false).context("Failed to open ALSA device")?;

    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(Format::s32())?;
        hwp.set_channels(1)?;
        hwp.set_rate(rate, ValueOr::Nearest)?;
        hwp.set_buffer_time_near(latency_us, ValueOr::Nearest)?;
        hwp.set_period_time_near(latency_us / 4, ValueOr::Nearest)?;
        pcm.hw_params(&hwp)?;
    }

    pcm.prepare().context("PCM prepare failed")?;
    Ok(pcm)
}

fn is_eof(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::UnexpectedEof)
}

#[cfg(test)]
mod tests {
    use super::{Frame, NO_PTS, read_frame};
    use byteorder::{BigEndian, ByteOrder};
    use tokio::io::AsyncWriteExt;

    fn header(pts: u64, len: u32) -> [u8; 12] {
        let mut out = [0u8; 12];
        BigEndian::write_u64(&mut out[..8], pts);
        BigEndian::write_u32(&mut out[8..12], len);
        out
    }

    #[tokio::test]
    async fn config_frame_does_not_consume_following_audio_frame() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        let writer_task = tokio::spawn(async move {
            writer.write_all(&header(NO_PTS, 2)).await.unwrap();
            writer.write_all(&[0x12, 0x10]).await.unwrap();
            writer.write_all(&header(7, 3)).await.unwrap();
            writer.write_all(&[1, 2, 3]).await.unwrap();
        });

        let config = read_frame(&mut reader).await.unwrap();
        let audio = read_frame(&mut reader).await.unwrap();
        writer_task.await.unwrap();

        assert_eq!(config, Frame::Config(vec![0x12, 0x10]));
        assert_eq!(
            audio,
            Frame::Audio {
                pts: 7,
                data: vec![1, 2, 3],
            }
        );
    }
}

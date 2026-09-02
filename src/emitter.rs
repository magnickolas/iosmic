use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use byteorder::{BigEndian, ByteOrder};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::debug::TimingDebug;
use crate::decode::{AacDecoder, DecodedAudio};
use crate::error::ConfigurationError;
use crate::policy::PlayPolicy;
use crate::renderer::{RendererEvent, RendererHandle};
use crate::sink::SinkFactory;

pub(crate) const AUDIO_REQ: &[u8] = b"GET /v1/audio.2";
const NO_PTS: u64 = u64::MAX;
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run<S>(
    mut stream: S,
    policy: PlayPolicy,
    sink_factory: Arc<dyn SinkFactory>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(AUDIO_REQ)
        .await
        .context("failed to send audio request")?;

    let timing = TimingDebug::from_env();
    let mut config = read_initial_config(&mut stream).await?;
    let mut codec_reset_count = 0u64;
    loop {
        let mut decoder = new_decoder(&config)?;
        let (first, first_pts) =
            read_first_decoded(&mut stream, &mut decoder, &mut config, &timing).await?;
        let generation = DecodedFormat::from(&first);
        let mut renderer = RendererHandle::start(
            Arc::clone(&sink_factory),
            policy.clone(),
            generation.sample_rate,
            generation.frames,
            timing.clone(),
        )?;
        insert_decoded(&renderer, first, &timing, first_pts)?;

        let next_config = run_generation(
            &mut stream,
            &mut decoder,
            &mut renderer,
            &config,
            generation,
            &timing,
        )
        .await;
        renderer.stop();

        match next_config? {
            Some(changed) => {
                codec_reset_count += 1;
                timing.log_render(format_args!("codec_reset_count={codec_reset_count}"));
                config = changed;
            }
            None => return Ok(()),
        }
    }
}

async fn run_generation<S>(
    stream: &mut S,
    decoder: &mut AacDecoder,
    renderer: &mut RendererHandle,
    config: &[u8],
    generation: DecodedFormat,
    timing: &TimingDebug,
) -> Result<Option<Vec<u8>>>
where
    S: AsyncRead + Unpin,
{
    loop {
        enum Input {
            Frame(Result<Frame>),
            Renderer(Option<RendererEvent>),
        }

        let input = tokio::select! {
            frame = tokio::time::timeout(IDLE_TIMEOUT, read_frame(stream)) => {
                Input::Frame(match frame {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!("no data received for {}s", IDLE_TIMEOUT.as_secs()),
                })
            }
            event = renderer.next_event() => Input::Renderer(event),
        };

        match input {
            Input::Renderer(Some(RendererEvent::Starvation)) => {
                anyhow::bail!("continuous decoded-audio starvation reached the reconnect threshold")
            }
            Input::Renderer(Some(RendererEvent::Terminal(error))) => return Err(error),
            Input::Renderer(None) => anyhow::bail!("playback thread stopped unexpectedly"),
            Input::Frame(Err(error)) if is_eof(&error) => {
                anyhow::bail!("audio transport ended")
            }
            Input::Frame(Err(error)) => return Err(error),
            Input::Frame(Ok(Frame::Config(changed))) => {
                let identical = changed == config;
                timing.log_config(identical, changed.len());
                if !identical {
                    return Ok(Some(changed));
                }
            }
            Input::Frame(Ok(Frame::Audio { pts, data })) => {
                let decoded = decoder.decode(&data)?;
                if decoded.frames == 0 {
                    continue;
                }
                let actual = DecodedFormat::from(&decoded);
                if actual.sample_rate != generation.sample_rate
                    || actual.channels != generation.channels
                {
                    return Err(ConfigurationError::new(format!(
                        "decoded format changed without a codec config marker: {generation:?} -> {actual:?}"
                    ))
                    .into());
                }
                insert_decoded(renderer, decoded, timing, pts)?;
            }
        }
    }
}

async fn read_initial_config<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    loop {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, read_frame(stream))
            .await
            .map_err(|_| {
                anyhow::anyhow!("no config received within {}s", IDLE_TIMEOUT.as_secs())
            })??;
        if let Frame::Config(config) = frame {
            return Ok(config);
        }
    }
}

async fn read_first_decoded<S>(
    stream: &mut S,
    decoder: &mut AacDecoder,
    config: &mut Vec<u8>,
    timing: &TimingDebug,
) -> Result<(DecodedAudio, u64)>
where
    S: AsyncRead + Unpin,
{
    loop {
        let frame = tokio::time::timeout(IDLE_TIMEOUT, read_frame(stream))
            .await
            .map_err(|_| {
                anyhow::anyhow!("no audio received within {}s", IDLE_TIMEOUT.as_secs())
            })??;
        match frame {
            Frame::Config(changed) => {
                let identical = changed == *config;
                timing.log_config(identical, changed.len());
                if !identical {
                    *config = changed;
                    *decoder = new_decoder(config)?;
                }
            }
            Frame::Audio { pts, data } => {
                let decoded = decoder.decode(&data)?;
                if decoded.frames > 0 {
                    return Ok((decoded, pts));
                }
            }
        }
    }
}

fn new_decoder(config: &[u8]) -> Result<AacDecoder> {
    AacDecoder::new(config).map_err(|error| {
        ConfigurationError::new(format!("unsupported AAC configuration: {error:#}")).into()
    })
}

fn insert_decoded(
    renderer: &RendererHandle,
    decoded: DecodedAudio,
    timing: &TimingDebug,
    pts: u64,
) -> Result<()> {
    let frames = decoded.frames;
    let sample_rate = decoded.sample_rate;
    let mono = downmix_mono(&decoded.samples, decoded.channels);
    anyhow::ensure!(
        mono.len() == frames,
        "decoded frame/channel accounting mismatch: {} mono frames, expected {frames}",
        mono.len()
    );
    renderer.insert(mono)?;
    timing.log_packet(pts, frames, sample_rate);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodedFormat {
    sample_rate: u32,
    channels: usize,
    frames: usize,
}

impl From<&DecodedAudio> for DecodedFormat {
    fn from(decoded: &DecodedAudio) -> Self {
        Self {
            sample_rate: decoded.sample_rate,
            channels: decoded.channels,
            frames: decoded.frames,
        }
    }
}

pub(crate) fn downmix_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }
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
        .context("failed to read header")?;
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
            anyhow::bail!("stop/error from app side");
        }
        anyhow::ensure!(len > 0 && len <= 1024, "config packet size invalid: {len}");
        let mut config = vec![0u8; len as usize];
        stream
            .read_exact(&mut config)
            .await
            .context("failed to read config")?;
        return Ok(Frame::Config(config));
    }

    anyhow::ensure!(
        len > 0 && len <= 1024 * 1024,
        "data packet size invalid: {len}"
    );
    let mut data = vec![0u8; len as usize];
    stream
        .read_exact(&mut data)
        .await
        .context("failed to read audio data")?;
    Ok(Frame::Audio { pts, data })
}

fn is_eof(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == ErrorKind::UnexpectedEof)
}

#[cfg(test)]
mod tests {
    use super::{Frame, NO_PTS, downmix_mono, read_frame, read_initial_config};
    use byteorder::{BigEndian, ByteOrder};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn header(pts: u64, len: u32) -> [u8; 12] {
        let mut output = [0u8; 12];
        BigEndian::write_u64(&mut output[..8], pts);
        BigEndian::write_u32(&mut output[8..12], len);
        output
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
                data: vec![1, 2, 3]
            }
        );
    }

    #[tokio::test]
    async fn stop_sentinel_is_preserved() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(&header(NO_PTS, u32::MAX)).await.unwrap();
        drop(writer);
        assert!(
            read_frame(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("stop/error")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn initial_config_times_out() {
        let (mut reader, _writer) = tokio::io::duplex(64);
        let result =
            tokio::time::timeout(Duration::from_secs(10), read_initial_config(&mut reader))
                .await
                .expect("outer timeout should not fire");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no config received")
        );
    }

    #[test]
    fn downmix_mono_passthrough_for_single_channel() {
        let input = [0.1, 0.2, 0.3];
        assert_eq!(downmix_mono(&input, 1), input);
    }

    #[test]
    fn downmix_mono_averages_stereo_frames() {
        let input = [1.0, 0.0, 0.0, 1.0];
        assert_eq!(downmix_mono(&input, 2), vec![0.5, 0.5]);
    }
}

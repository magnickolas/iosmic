use anyhow::{Context, Result};
use byteorder::{BigEndian, ByteOrder};
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::debug::TimingDebug;
use crate::decode::AacDecoder;
use crate::policy::{PlayPolicy, PolicyState};
use crate::sink::AudioSink;

pub(crate) const AUDIO_REQ: &[u8] = b"GET /v1/audio.2";
const NO_PTS: u64 = u64::MAX;
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run<S, F>(mut stream: S, policy: PlayPolicy, open_sink: F) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(u32) -> Result<Box<dyn AudioSink>>,
{
    stream
        .write_all(AUDIO_REQ)
        .await
        .context("Failed to send audio request")?;

    let (mut decoder, mut state, mut timing_debug) =
        handshake(&mut stream, &policy, open_sink).await?;

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
            Frame::Config(_) => continue,
            Frame::Audio { pts, data } => (pts, data),
        };

        let samples = decoder.decode(&audio_data, pts)?;
        if samples.is_empty() {
            continue;
        }

        let out = downmix_mono(&samples, decoder.channels());
        if !out.is_empty() {
            let delay = state.write(&out)?;
            timing_debug.log(pts, out.len(), decoder.sample_rate(), delay);
        }
    }
}

pub(crate) async fn handshake<S, F>(
    stream: &mut S,
    policy: &PlayPolicy,
    open_sink: F,
) -> Result<(AacDecoder, PolicyState<Box<dyn AudioSink>>, TimingDebug)>
where
    S: AsyncRead + Unpin,
    F: FnOnce(u32) -> Result<Box<dyn AudioSink>>,
{
    loop {
        let frame = match tokio::time::timeout(IDLE_TIMEOUT, read_frame(stream)).await {
            Ok(Ok(frame)) => frame,
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("No config received within {}s", IDLE_TIMEOUT.as_secs()),
        };
        match frame {
            Frame::Config(config) => {
                let decoder = AacDecoder::new(&config)?;
                let buffer_us = policy.buffer_us();
                eprintln!(
                    "input {}Hz {}ch, buffer {}ms",
                    decoder.sample_rate(),
                    decoder.channels(),
                    buffer_us / 1000
                );
                let sink = open_sink(decoder.sample_rate())?;
                let state = PolicyState::new(policy, decoder.sample_rate(), 1024, sink)?;
                let timing_debug = TimingDebug::from_env();
                return Ok((decoder, state, timing_debug));
            }
            Frame::Audio { .. } => continue,
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

fn is_eof(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::UnexpectedEof)
}

#[cfg(test)]
mod tests {
    use super::{Frame, NO_PTS, downmix_mono, handshake, read_frame};
    use crate::policy::PlayPolicy;
    use crate::sink::{AudioSink, MockAudioSink};
    use byteorder::{BigEndian, ByteOrder};
    use std::time::Duration;
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

    #[tokio::test]
    async fn read_frame_stop_signal_is_rejected() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(&header(NO_PTS, u32::MAX)).await.unwrap();
        drop(writer);

        let result = read_frame(&mut reader).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Stop/error from app side"),
            "expected 'Stop/error from app side' in error"
        );
    }

    #[tokio::test]
    async fn read_frame_config_zero_length_is_rejected() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(&header(NO_PTS, 0)).await.unwrap();
        drop(writer);

        let result = read_frame(&mut reader).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Config packet size invalid"),
            "expected 'Config packet size invalid' in error"
        );
    }

    #[test]
    fn downmix_mono_passthrough_for_single_channel() {
        let input = [0.1f32, 0.2, 0.3];
        let output = downmix_mono(&input, 1);
        assert_eq!(output, input.to_vec());
    }

    #[test]
    fn downmix_mono_averages_stereo_frames() {
        let input = [1.0f32, 0.0, 0.0, 1.0];
        let output = downmix_mono(&input, 2);
        assert_eq!(output, vec![0.5f32, 0.5]);
    }

    #[tokio::test]
    async fn handshake_succeeds_with_valid_config_frame() {
        let (mut reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(&header(NO_PTS, 2)).await.unwrap();
        writer.write_all(&[0x12, 0x10]).await.unwrap();
        drop(writer);

        let policy = PlayPolicy {
            buffer_ms: 50,
            latency_reconnect_ms: 0,
        };
        let open_sink = |_sample_rate: u32| -> anyhow::Result<Box<dyn AudioSink>> {
            Ok(Box::new(MockAudioSink::new()))
        };
        let (decoder, _state, _timing_debug) =
            handshake(&mut reader, &policy, open_sink).await.unwrap();

        assert_eq!(decoder.sample_rate(), 44100);
        assert_eq!(decoder.channels(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_times_out_when_no_data() {
        let (mut reader, _writer) = tokio::io::duplex(64);
        let policy = PlayPolicy {
            buffer_ms: 50,
            latency_reconnect_ms: 0,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            handshake(
                &mut reader,
                &policy,
                |_: u32| -> anyhow::Result<Box<dyn AudioSink>> { panic!("should not open sink") },
            ),
        )
        .await
        .expect("outer timeout should not fire");
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("No config received"),
            "expected 'No config received' in error"
        );
    }
}

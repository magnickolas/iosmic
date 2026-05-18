use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use byteorder::{BigEndian, ByteOrder};
use std::env;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::connection::Stream;
use crate::decode::AacDecoder;
use crate::policy::{PlayPolicy, PolicyState, delay_samples};

const AUDIO_REQ: &[u8] = b"GET /v1/audio.2";
const NO_PTS: u64 = u64::MAX;

pub async fn run(mut stream: Stream, policy: PlayPolicy, sink_device: &str) -> Result<()> {
    stream
        .write_all(AUDIO_REQ)
        .await
        .context("Failed to send audio request")?;

    let mut decoder: Option<AacDecoder> = None;
    let mut pcm: Option<PCM> = None;
    let mut state: Option<PolicyState> = None;
    let mut pts_debug = PtsDebug::from_env();
    let mut timing_debug = TimingDebug::from_env();

    loop {
        let frame = read_frame(&mut stream).await?;
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
            eprintln!(
                "input {}Hz {}ch, buffer {}ms",
                dec.sample_rate(),
                dec.channels(),
                buffer_us / 1000
            );
            pcm = Some(open_pcm(sink_device, dec.sample_rate(), buffer_us)?);
            state = Some(PolicyState::new(&policy, dec.sample_rate()));
        }

        let dec = decoder.as_mut().context("Got audio data before config")?;
        let samples = dec.decode(&audio_data, pts)?;
        if samples.is_empty() {
            continue;
        }
        pts_debug.log(
            pts,
            samples.len() / dec.channels(),
            audio_data.len(),
            dec.sample_rate(),
        );

        let (Some(p), Some(st)) = (pcm.as_ref(), state.as_mut()) else {
            continue;
        };

        let out = f32_to_s32_mono(&samples, dec.channels());
        if !out.is_empty() {
            timing_debug.log(pts, out.len(), dec.sample_rate(), delay_samples(p));
            st.write(p, &out)?;
        }
    }
}

pub async fn measure(mut stream: Stream, duration: Duration) -> Result<()> {
    stream
        .write_all(AUDIO_REQ)
        .await
        .context("Failed to send audio request")?;

    let warmup = measurement_warmup(duration);
    let started = Instant::now();
    let measure_after = started + warmup;
    let deadline = started + duration;
    let mut decoder: Option<AacDecoder> = None;
    let mut stats = MeasureStats::default();

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let frame = match tokio::time::timeout(remaining, read_frame(&mut stream)).await {
            Ok(frame) => frame?,
            Err(_) => break,
        };
        let (pts, audio_data) = match frame {
            Frame::Config(config) => {
                decoder = Some(AacDecoder::new(&config)?);
                continue;
            }
            Frame::Audio { pts, data } => (pts, data),
        };

        let dec = decoder.as_mut().context("Got audio data before config")?;
        let samples = dec.decode(&audio_data, pts)?;
        if samples.is_empty() {
            continue;
        }

        if Instant::now() < measure_after {
            continue;
        }

        stats.record(samples.len() / dec.channels(), dec.sample_rate());
    }

    stats.print_report(duration, warmup)
}

#[derive(Default)]
struct MeasureStats {
    frame_count: u64,
    prev_arrival: Option<Instant>,
    positive_drift_ms: Vec<f64>,
    max_drift_ms: f64,
}

impl MeasureStats {
    fn record(&mut self, decoded_frames: usize, sample_rate: u32) {
        let now = Instant::now();
        self.frame_count += 1;

        if let Some(prev) = self.prev_arrival {
            let arrival_delta_ms = now.duration_since(prev).as_secs_f64() * 1000.0;
            let decoded_ms = decoded_frames as f64 * 1000.0 / sample_rate as f64;
            let drift_ms = arrival_delta_ms - decoded_ms;
            let positive_drift_ms = drift_ms.max(0.0);

            self.positive_drift_ms.push(positive_drift_ms);
            self.max_drift_ms = self.max_drift_ms.max(positive_drift_ms);
        }

        self.prev_arrival = Some(now);
    }

    fn print_report(&mut self, duration: Duration, warmup: Duration) -> Result<()> {
        anyhow::ensure!(
            !self.positive_drift_ms.is_empty(),
            "Not enough audio packets received to measure jitter"
        );

        self.positive_drift_ms
            .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p50 = percentile(&self.positive_drift_ms, 0.50);
        let p95 = percentile(&self.positive_drift_ms, 0.95);
        let p99 = percentile(&self.positive_drift_ms, 0.99);
        let measured = duration.saturating_sub(warmup);

        println!("total_seconds={:.1}", duration.as_secs_f64());
        println!("warmup_seconds={:.1}", warmup.as_secs_f64());
        println!("measurement_seconds={:.1}", measured.as_secs_f64());
        println!("audio_packets={}", self.frame_count);
        println!("positive_drift_p50_ms={p50:.3}");
        println!("positive_drift_p95_ms={p95:.3}");
        println!("positive_drift_p99_ms={p99:.3}");
        println!("positive_drift_max_ms={:.3}", self.max_drift_ms);

        Ok(())
    }
}

fn percentile(sorted_values: &[f64], percentile: f64) -> f64 {
    debug_assert!(!sorted_values.is_empty());
    let last = sorted_values.len() - 1;
    let idx = (last as f64 * percentile).ceil() as usize;
    sorted_values[idx.min(last)]
}

fn measurement_warmup(duration: Duration) -> Duration {
    Duration::from_secs_f64((duration.as_secs_f64() / 10.0).min(2.0))
}

struct TimingDebug {
    enabled: bool,
    frame_count: u64,
    prev_arrival: Option<Instant>,
    audio_frames_seen: u64,
}

impl TimingDebug {
    fn from_env() -> Self {
        Self {
            enabled: env::var_os("IOSMIC_TIMING_DEBUG").is_some(),
            frame_count: 0,
            prev_arrival: None,
            audio_frames_seen: 0,
        }
    }

    fn log(&mut self, pts: u64, decoded_frames: usize, sample_rate: u32, alsa_delay_samples: i64) {
        if !self.enabled {
            return;
        }

        let now = Instant::now();
        self.frame_count += 1;
        let arrival_delta_ms = self
            .prev_arrival
            .map(|prev| now.duration_since(prev).as_secs_f64() * 1000.0);
        let decoded_ms = decoded_frames as f64 * 1000.0 / sample_rate as f64;
        let drift_ms = arrival_delta_ms.map(|arrival| arrival - decoded_ms);
        let alsa_delay_ms = alsa_delay_samples as f64 * 1000.0 / sample_rate as f64;
        self.audio_frames_seen += decoded_frames as u64;

        eprintln!(
            "timing frame={} pts={} arrival_delta_ms={:.3?} decoded_frames={} decoded_ms={:.3} drift_ms={:.3?} audio_seen_ms={:.3} alsa_delay_ms={:.3}",
            self.frame_count,
            pts,
            arrival_delta_ms,
            decoded_frames,
            decoded_ms,
            drift_ms,
            self.audio_frames_seen as f64 * 1000.0 / sample_rate as f64,
            alsa_delay_ms
        );

        self.prev_arrival = Some(now);
    }
}

fn f32_to_s32_mono(samples: &[f32], channels: usize) -> Vec<i32> {
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let mut sum = 0.0f32;
        for ch in 0..channels {
            sum += samples[i * channels + ch];
        }
        sum /= channels as f32;
        out.push((sum * i32::MAX as f32) as i32);
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Frame {
    Config(Vec<u8>),
    Audio { pts: u64, data: Vec<u8> },
}

async fn read_frame<R>(stream: &mut R) -> Result<Frame>
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

struct PtsDebug {
    enabled: bool,
    frame_count: u64,
    prev_pts: Option<u64>,
    prev_decoded_frames: Option<usize>,
}

impl PtsDebug {
    fn from_env() -> Self {
        Self {
            enabled: env::var_os("IOSMIC_PTS_DEBUG").is_some(),
            frame_count: 0,
            prev_pts: None,
            prev_decoded_frames: None,
        }
    }

    fn log(&mut self, pts: u64, decoded_frames: usize, packet_bytes: usize, sample_rate: u32) {
        if !self.enabled {
            return;
        }

        self.frame_count += 1;
        let pts_delta = self.prev_pts.map(|prev| pts.saturating_sub(prev));
        let expected_delta = self.prev_decoded_frames;
        let delta_ms = pts_delta.map(|delta| delta as f64 * 1000.0 / sample_rate as f64);
        let decoded_ms = decoded_frames as f64 * 1000.0 / sample_rate as f64;
        let status = match (pts_delta, expected_delta) {
            (Some(delta), Some(expected)) if delta == expected as u64 => "match",
            (Some(_), Some(_)) => "mismatch",
            _ => "first",
        };

        eprintln!(
            "pts frame={} pts={} pts_delta={:?} pts_delta_ms={:.3?} decoded_frames={} decoded_ms={:.3} packet_bytes={} {}",
            self.frame_count,
            pts,
            pts_delta,
            delta_ms,
            decoded_frames,
            decoded_ms,
            packet_bytes,
            status
        );

        self.prev_pts = Some(pts);
        self.prev_decoded_frames = Some(decoded_frames);
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, NO_PTS, measurement_warmup, percentile, read_frame};
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

    #[test]
    fn percentile_uses_nearest_upper_sample() {
        let values = [0.0, 1.0, 2.0, 3.0, 4.0];

        assert_eq!(percentile(&values, 0.50), 2.0);
        assert_eq!(percentile(&values, 0.95), 4.0);
        assert_eq!(percentile(&values, 0.99), 4.0);
    }

    #[test]
    fn measurement_warmup_is_capped_to_short_startup_window() {
        assert_eq!(
            measurement_warmup(Duration::from_secs(5)),
            Duration::from_millis(500)
        );
        assert_eq!(
            measurement_warmup(Duration::from_secs(30)),
            Duration::from_secs(2)
        );
    }
}

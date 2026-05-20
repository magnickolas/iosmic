use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

use crate::connection::Stream;
use crate::decode::AacDecoder;
use crate::emitter::{Frame, read_frame, AUDIO_REQ};

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
        let p999 = percentile(&self.positive_drift_ms, 0.999);
        let measured = duration.saturating_sub(warmup);

        println!("total_seconds={:.1}", duration.as_secs_f64());
        println!("warmup_seconds={:.1}", warmup.as_secs_f64());
        println!("measurement_seconds={:.1}", measured.as_secs_f64());
        println!("audio_packets={}", self.frame_count);
        println!("positive_drift_p50_ms={p50:.3}");
        println!("positive_drift_p95_ms={p95:.3}");
        println!("positive_drift_p99_ms={p99:.3}");
        println!("positive_drift_p999_ms={p999:.3}");
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

#[cfg(test)]
mod tests {
    use super::{measurement_warmup, percentile};
    use std::time::Duration;

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

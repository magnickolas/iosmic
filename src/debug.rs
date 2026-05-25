use std::env;
use std::time::Instant;

pub struct TimingDebug {
    enabled: bool,
    frame_count: u64,
    prev_arrival: Option<Instant>,
    audio_frames_seen: u64,
}

impl TimingDebug {
    pub fn from_env() -> Self {
        Self {
            enabled: env::var_os("IOSMIC_TIMING_DEBUG").is_some(),
            frame_count: 0,
            prev_arrival: None,
            audio_frames_seen: 0,
        }
    }

    pub fn log(
        &mut self,
        pts: u64,
        decoded_frames: usize,
        sample_rate: u32,
        alsa_delay_samples: i64,
    ) {
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

        let arrival_str = arrival_delta_ms.map_or("-".to_string(), |v| format!("{v:.3}"));
        let drift_str = drift_ms.map_or("-".to_string(), |v| format!("{v:.3}"));

        eprintln!(
            "timing frame={} pts={} arrival_delta_ms={} decoded_frames={} decoded_ms={:.3} drift_ms={} audio_seen_ms={:.3} alsa_delay_ms={:.3}",
            self.frame_count,
            pts,
            arrival_str,
            decoded_frames,
            decoded_ms,
            drift_str,
            self.audio_frames_seen as f64 * 1000.0 / sample_rate as f64,
            alsa_delay_ms
        );

        self.prev_arrival = Some(now);
    }
}

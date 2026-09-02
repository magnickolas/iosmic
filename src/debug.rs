use std::env;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone)]
pub struct TimingDebug {
    enabled: bool,
    state: Arc<Mutex<PacketTimingState>>,
}

#[derive(Default)]
struct PacketTimingState {
    frame_count: u64,
    prev_arrival: Option<Instant>,
    previous_pts: Option<u64>,
    audio_duration_seconds: f64,
}

impl TimingDebug {
    pub fn from_env() -> Self {
        Self {
            enabled: env::var_os("IOSMIC_TIMING_DEBUG").is_some(),
            state: Arc::new(Mutex::new(PacketTimingState::default())),
        }
    }

    pub fn log_packet(&self, pts: u64, decoded_frames: usize, sample_rate: u32) {
        if !self.enabled {
            return;
        }

        let now = Instant::now();
        let mut state = self.state.lock().expect("timing debug mutex poisoned");
        state.frame_count += 1;
        let arrival_delta_ms = state
            .prev_arrival
            .map(|previous| now.duration_since(previous).as_secs_f64() * 1000.0);
        let pts_delta = state
            .previous_pts
            .map(|previous| pts.wrapping_sub(previous));
        let decoded_ms = decoded_frames as f64 * 1000.0 / sample_rate as f64;
        state.audio_duration_seconds += decoded_frames as f64 / sample_rate as f64;

        eprintln!(
            "timing packet={} raw_pts_untrusted={} raw_pts_delta={} arrival_delta_ms={} decoded_frames={} decoded_ms={decoded_ms:.3} audio_seen_ms={:.3}",
            state.frame_count,
            pts,
            pts_delta.map_or_else(|| "-".to_owned(), |value| value.to_string()),
            arrival_delta_ms.map_or_else(|| "-".to_owned(), |value| format!("{value:.3}")),
            decoded_frames,
            state.audio_duration_seconds * 1000.0,
        );

        state.prev_arrival = Some(now);
        state.previous_pts = Some(pts);
    }

    pub fn log_config(&self, identical: bool, bytes: usize) {
        if self.enabled {
            eprintln!("timing codec_config_bytes={bytes} identical_repeat={identical}");
        }
    }

    pub fn log_render(&self, message: impl std::fmt::Display) {
        if self.enabled {
            eprintln!("timing {message}");
        }
    }
}

use anyhow::Result;

use crate::resample::AdaptiveResampler;
use crate::sink::AudioSink;

#[derive(Clone)]
pub struct PlayPolicy {
    pub buffer_ms: u32,
    pub latency_reconnect_ms: u32,
}

impl PlayPolicy {
    pub fn buffer_us(&self) -> u32 {
        self.buffer_ms * 1000
    }
}

pub struct PolicyState<S> {
    sink: S,
    prefill_buf: Vec<f32>,
    prefilled: bool,
    buffer_samples: u32,
    latency_reconnect_samples: i64,
    resampler: AdaptiveResampler,
}

impl<S: AudioSink> PolicyState<S> {
    pub fn new(policy: &PlayPolicy, sample_rate: u32, chunk_size: usize, sink: S) -> Result<Self> {
        let buffer_samples = (sample_rate as u64 * policy.buffer_ms as u64 / 1000) as u32;
        let latency_reconnect_samples =
            (sample_rate as u64 * policy.latency_reconnect_ms as u64 / 1000) as i64;
        let resampler = AdaptiveResampler::new(chunk_size, buffer_samples)?;
        Ok(Self {
            sink,
            prefill_buf: Vec::new(),
            prefilled: false,
            buffer_samples,
            latency_reconnect_samples,
            resampler,
        })
    }

    /// Write samples through the playback pipeline. Returns the current delay in samples.
    pub fn write(&mut self, samples: &[f32]) -> Result<i64> {
        if !self.prefilled {
            self.prefill_buf.extend_from_slice(samples);
            if self.prefill_buf.len() >= self.buffer_samples as usize {
                let s32: Vec<i32> = self.prefill_buf.iter().map(|&s| f32_to_s32(s)).collect();
                self.sink.write(&s32)?;
                self.prefill_buf = Vec::new();
                self.prefilled = true;
            }
            return Ok(self.sink.delay_samples());
        }

        let delay = self.sink.delay_samples();
        if self.latency_reconnect_samples > 0 && delay > self.latency_reconnect_samples {
            anyhow::bail!("Latency {delay} samples exceeds reconnect threshold");
        }
        let resampled = self.resampler.process(samples, delay)?;
        let s32: Vec<i32> = resampled.iter().map(|&s| f32_to_s32(s)).collect();
        self.sink.write(&s32)?;
        Ok(delay)
    }
}

fn f32_to_s32(s: f32) -> i32 {
    (s * i32::MAX as f32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::MockAudioSink;
    use proptest::prelude::*;

    fn default_policy(buffer_ms: u32, latency_reconnect_ms: u32) -> PlayPolicy {
        PlayPolicy {
            buffer_ms,
            latency_reconnect_ms,
        }
    }

    // Helper: build a PolicyState with a pre-configured mock.
    fn make_state(
        policy: &PlayPolicy,
        sample_rate: u32,
        chunk_size: usize,
        sink: MockAudioSink,
    ) -> PolicyState<MockAudioSink> {
        PolicyState::new(policy, sample_rate, chunk_size, sink).expect("PolicyState::new failed")
    }

    // Helper: flush the prefill phase by writing `buffer_samples` zeros.
    fn flush_prefill(state: &mut PolicyState<MockAudioSink>, buffer_samples: usize) {
        let zeros = vec![0.0f32; buffer_samples];
        state.write(&zeros).expect("prefill flush failed");
    }

    #[test]
    fn prefill_accumulates_without_flushing_until_threshold() {
        // buffer_ms=100, rate=1000 → buffer_samples=100
        let policy = default_policy(100, 0);
        let mut mock = MockAudioSink::new();
        // sink.write must never be called during accumulation
        mock.expect_write().times(0);
        // delay_samples is called after the early return in prefill path
        mock.expect_delay_samples().returning(|| 42i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        let samples = vec![0.0f32; 50];
        let result = state.write(&samples);
        assert!(result.is_ok());
    }

    #[test]
    fn prefill_flushes_exactly_at_threshold() {
        // buffer_ms=100, rate=1000 → buffer_samples=100
        let policy = default_policy(100, 0);
        let mut mock = MockAudioSink::new();
        // write should be called exactly once with 100 i32 values
        mock.expect_write()
            .times(1)
            .withf(|data| data.len() == 100)
            .returning(|_| Ok(()));
        mock.expect_delay_samples().returning(|| 42i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        let samples = vec![0.0f32; 100];
        let result = state.write(&samples);
        assert!(result.is_ok());
    }

    #[test]
    fn prefill_flush_then_subsequent_write_goes_through_resampler() {
        // buffer_ms=10, rate=1000 → buffer_samples=10
        // chunk_size=1024 must match AdaptiveResampler and the post-prefill write length
        let policy = default_policy(10, 0);
        let mut mock = MockAudioSink::new();
        // First call: prefill flush (10 samples); second call: resampled output
        mock.expect_write()
            .times(2)
            .with(mockall::predicate::always())
            .returning(|_| Ok(()));
        mock.expect_delay_samples().returning(|| 42i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        // Flush prefill
        flush_prefill(&mut state, 10);

        // Post-prefill write: must be exactly chunk_size=1024 to match AdaptiveResampler
        let samples = vec![0.0f32; 1024];
        let result = state.write(&samples);
        assert!(result.is_ok());
    }

    #[test]
    fn reconnect_threshold_zero_never_bails() {
        // latency_reconnect_ms=0 → threshold=0 → no bail regardless of delay
        let policy = default_policy(10, 0);
        let mut mock = MockAudioSink::new();
        mock.expect_write()
            .times(2)
            .with(mockall::predicate::always())
            .returning(|_| Ok(()));
        mock.expect_delay_samples().returning(|| 999999i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        flush_prefill(&mut state, 10);

        let samples = vec![0.0f32; 1024];
        let result = state.write(&samples);
        assert!(result.is_ok());
    }

    #[test]
    fn reconnect_threshold_exceeded_returns_error() {
        // latency_reconnect_ms=50, rate=1000 → threshold=50; delay=51 → bail
        let policy = default_policy(10, 50);
        let mut mock = MockAudioSink::new();
        // write is called once for prefill; then write is NOT called (bail before resampler)
        mock.expect_write()
            .times(1)
            .with(mockall::predicate::always())
            .returning(|_| Ok(()));
        mock.expect_delay_samples().returning(|| 51i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        flush_prefill(&mut state, 10);

        let samples = vec![0.0f32; 1024];
        let result = state.write(&samples);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("Latency"), "error message was: {msg}");
    }

    #[test]
    fn reconnect_threshold_at_boundary_does_not_bail() {
        // threshold=50 samples; delay=50 → condition is `delay > threshold`, so no bail
        let policy = default_policy(10, 50);
        let mut mock = MockAudioSink::new();
        mock.expect_write()
            .times(2)
            .with(mockall::predicate::always())
            .returning(|_| Ok(()));
        mock.expect_delay_samples().returning(|| 50i64);

        let mut state = make_state(&policy, 1000, 1024, mock);

        flush_prefill(&mut state, 10);

        let samples = vec![0.0f32; 1024];
        let result = state.write(&samples);
        assert!(result.is_ok());
    }

    proptest! {
        #[test]
        fn write_does_not_panic_for_arbitrary_samples(
            raw in prop::collection::vec(-1.0f32..=1.0f32, 1024)
        ) {
            let policy = default_policy(10, 0);
            let mut mock = MockAudioSink::new();
            mock.expect_write()
                .with(mockall::predicate::always())
                .returning(|_| Ok(()));
            mock.expect_delay_samples().returning(|| 42i64);

            let mut state = make_state(&policy, 1000, 1024, mock);

            flush_prefill(&mut state, 10);

            let result = state.write(&raw);
            prop_assert!(result.is_ok());
        }
    }
}

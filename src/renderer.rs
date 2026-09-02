use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::debug::TimingDebug;
use crate::error::ConfigurationError;
use crate::jitter::{EvictionStats, JitterBuffer, TakenAudio};
use crate::policy::{
    BufferLimits, MAX_GROUP_DELAY, PlayPolicy, REQUESTED_PERIOD, WRITE_ATTEMPT,
    WRITE_EXPIRY_ATTEMPTS, duration_to_frames_ceil,
};
use crate::resample::{FixedOutputResampler, OccupancyController};
use crate::sink::{
    AudioSink, Availability, MissingPulsePlugin, SinkFactory, SinkParameters, WaitResult,
    WriteResult,
};

const REOPEN_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const REOPEN_MAX_BACKOFF: Duration = Duration::from_secs(5);
const REOPEN_DIAGNOSTIC_TIMES: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];
const DECLICK: Duration = Duration::from_millis(5);

#[derive(Debug)]
pub enum RendererEvent {
    Starvation,
    Terminal(anyhow::Error),
}

struct SharedAudio {
    buffer: JitterBuffer,
    limits: BufferLimits,
    source_rate: u32,
    largest_packet_frames: usize,
    period: Duration,
    age_epoch: Instant,
    started: bool,
    splice_serial: u64,
    limits_serial: u64,
}

impl SharedAudio {
    fn current_index(&self, now: Instant) -> u64 {
        period_index(now.saturating_duration_since(self.age_epoch), self.period)
    }

    fn refresh_splice_serial(&mut self, before: EvictionStats) {
        if self.buffer.eviction_stats() != before {
            self.splice_serial = self.splice_serial.wrapping_add(1);
        }
    }

    fn clear_and_reanchor(&mut self, now: Instant) {
        self.buffer.clear();
        self.started = false;
        self.splice_serial = self.splice_serial.wrapping_add(1);
        self.age_epoch = now;
    }
}

pub struct RendererHandle {
    shared: Arc<Mutex<SharedAudio>>,
    policy: PlayPolicy,
    cancel: Arc<AtomicBool>,
    events: mpsc::Receiver<RendererEvent>,
    thread: Option<JoinHandle<()>>,
}

impl RendererHandle {
    pub fn start(
        factory: Arc<dyn SinkFactory>,
        policy: PlayPolicy,
        source_rate: u32,
        first_packet_frames: usize,
        timing: TimingDebug,
    ) -> Result<Self> {
        let parameters = SinkParameters {
            rate: source_rate,
            period_frames: duration_to_frames_ceil(REQUESTED_PERIOD, source_rate),
            buffer_frames: duration_to_frames_ceil(policy.requested_alsa_buffer(), source_rate),
        };
        let limits = policy.derive_limits(source_rate, first_packet_frames, parameters.period())?;
        let resampler = checked_resampler(source_rate, parameters)?;
        let controller = OccupancyController::new(
            source_rate,
            limits.target_frames,
            limits.largest_packet_frames,
            parameters.period(),
        );
        let shared = Arc::new(Mutex::new(SharedAudio {
            buffer: JitterBuffer::new(limits.maximum_frames, limits.maximum_age_periods),
            limits,
            source_rate,
            largest_packet_frames: first_packet_frames,
            period: parameters.period(),
            age_epoch: Instant::now(),
            started: false,
            splice_serial: 0,
            limits_serial: 0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (event_tx, events) = mpsc::channel(4);
        let thread_shared = Arc::clone(&shared);
        let thread_cancel = Arc::clone(&cancel);
        let thread_policy = policy.clone();
        let thread_timing = timing.clone();
        let thread = std::thread::Builder::new()
            .name("iosmic-render".to_owned())
            .spawn(move || {
                let mut worker = RenderWorker::new(
                    None,
                    factory,
                    thread_policy,
                    source_rate,
                    thread_shared,
                    thread_cancel,
                    event_tx,
                    resampler,
                    controller,
                    thread_timing,
                );
                if let Err(error) = worker.run() {
                    worker.send_event(RendererEvent::Terminal(error));
                }
            })
            .context("failed to spawn playback thread")?;

        Ok(Self {
            shared,
            policy,
            cancel,
            events,
            thread: Some(thread),
        })
    }

    pub fn insert(&self, samples: Vec<f32>) -> Result<()> {
        let mut shared = self.shared.lock().expect("jitter buffer mutex poisoned");
        let now = Instant::now();
        let index = shared.current_index(now);
        let packet_frames = samples.len();
        let largest = shared.largest_packet_frames.max(packet_frames);
        if largest != shared.largest_packet_frames {
            let limits = self
                .policy
                .derive_limits(shared.source_rate, largest, shared.period)?;
            shared
                .buffer
                .update_limits(limits.maximum_frames, limits.maximum_age_periods, index);
            shared.limits = limits;
            shared.largest_packet_frames = largest;
            shared.limits_serial = shared.limits_serial.wrapping_add(1);
            if shared.buffer.occupancy() < limits.target_frames {
                shared.started = false;
            }
        }

        let before = shared.buffer.eviction_stats();
        shared.buffer.insert(samples, index);
        shared.refresh_splice_serial(before);
        Ok(())
    }

    pub async fn next_event(&mut self) -> Option<RendererEvent> {
        self.events.recv().await
    }

    pub fn stop(mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RendererHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

struct InFlight {
    samples: Vec<i32>,
    offset: usize,
    oldest_insertion_index: Option<u64>,
}

struct RenderWorker {
    sink: Option<Box<dyn AudioSink>>,
    factory: Arc<dyn SinkFactory>,
    policy: PlayPolicy,
    source_rate: u32,
    shared: Arc<Mutex<SharedAudio>>,
    cancel: Arc<AtomicBool>,
    events: mpsc::Sender<RendererEvent>,
    resampler: FixedOutputResampler,
    controller: OccupancyController,
    in_flight: Option<InFlight>,
    consecutive_expiries: u32,
    underrun_recovery_started: Option<Instant>,
    rendered_source: bool,
    starving_for: Duration,
    source_was_silent: bool,
    last_output: f32,
    ramp_from: Option<f32>,
    seen_splice_serial: u64,
    seen_limits_serial: u64,
    timing: TimingDebug,
    completed_periods: u64,
    counters: RenderCounters,
    saturation_reported: bool,
}

#[derive(Default)]
struct RenderCounters {
    would_block: u64,
    write_expiries: u64,
    underruns: u64,
    hard_recoveries: u64,
    reopen_attempts: u64,
    reopen_failures: u64,
    splices: u64,
    silence_frames: u64,
}

impl RenderWorker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        sink: Option<Box<dyn AudioSink>>,
        factory: Arc<dyn SinkFactory>,
        policy: PlayPolicy,
        source_rate: u32,
        shared: Arc<Mutex<SharedAudio>>,
        cancel: Arc<AtomicBool>,
        events: mpsc::Sender<RendererEvent>,
        resampler: FixedOutputResampler,
        controller: OccupancyController,
        timing: TimingDebug,
    ) -> Self {
        Self {
            sink,
            factory,
            policy,
            source_rate,
            shared,
            cancel,
            events,
            resampler,
            controller,
            in_flight: None,
            consecutive_expiries: 0,
            underrun_recovery_started: None,
            rendered_source: false,
            starving_for: Duration::ZERO,
            source_was_silent: true,
            last_output: 0.0,
            ramp_from: None,
            seen_splice_serial: 0,
            seen_limits_serial: 0,
            timing,
            completed_periods: 0,
            counters: RenderCounters::default(),
            saturation_reported: false,
        }
    }

    fn run(&mut self) -> Result<()> {
        if self.sink.is_none() {
            self.reopen_sink(true)?;
        }
        while !self.cancel.load(Ordering::Acquire) {
            match self.write_one_attempt()? {
                AttemptResult::Completed => {
                    self.consecutive_expiries = 0;
                    self.underrun_recovery_started = None;
                    self.completed_periods += 1;
                    if self.completed_periods.is_multiple_of(100) {
                        self.log_period();
                    }
                }
                AttemptResult::Expired => {
                    self.counters.write_expiries += 1;
                    self.consecutive_expiries += 1;
                    if self.consecutive_expiries >= WRITE_EXPIRY_ATTEMPTS {
                        self.hard_write_recovery()?;
                    }
                }
                AttemptResult::Underrun => {
                    self.counters.underruns += 1;
                    self.recover_underrun()?;
                }
                AttemptResult::Reopen => self.reopen_sink(false)?,
            }
        }

        if let Some(sink) = self.sink.as_mut() {
            let _ = sink.drop_queue();
        }
        Ok(())
    }

    fn write_one_attempt(&mut self) -> Result<AttemptResult> {
        let deadline = Instant::now() + WRITE_ATTEMPT;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Ok(AttemptResult::Completed);
            }
            if self.in_flight_is_expired() {
                self.in_flight = None;
                self.mark_splice(false);
            }

            let now = Instant::now();
            let Some(remaining) = deadline.checked_duration_since(now) else {
                return Ok(AttemptResult::Expired);
            };
            let ready = match self
                .sink
                .as_ref()
                .expect("sink is open")
                .wait_ready(remaining)
            {
                Ok(WaitResult::Ready) => true,
                Ok(WaitResult::TimedOut) => false,
                Ok(WaitResult::Underrun) => return Ok(AttemptResult::Underrun),
                Ok(WaitResult::Suspended) => return Ok(AttemptResult::Reopen),
                Err(_) => return Ok(AttemptResult::Reopen),
            };
            if !ready {
                return Ok(AttemptResult::Expired);
            }
            let period_frames = self
                .sink
                .as_ref()
                .expect("sink is open")
                .parameters()
                .period_frames;
            match self.sink.as_ref().expect("sink is open").available_frames() {
                Ok(Availability::Frames(frames)) if frames < period_frames => continue,
                Ok(Availability::Frames(_)) => {}
                Ok(Availability::Underrun) => return Ok(AttemptResult::Underrun),
                Ok(Availability::Suspended) => return Ok(AttemptResult::Reopen),
                Err(_) => return Ok(AttemptResult::Reopen),
            }

            if self.in_flight.is_none() {
                self.in_flight = Some(self.render_period()?);
            }
            if self.in_flight_is_expired() {
                self.in_flight = None;
                self.mark_splice(false);
                continue;
            }

            let in_flight = self.in_flight.as_mut().expect("period was prepared");
            let write_result = match self
                .sink
                .as_mut()
                .expect("sink is open")
                .write(&in_flight.samples[in_flight.offset..])
            {
                Ok(result) => result,
                Err(_) => return Ok(AttemptResult::Reopen),
            };
            match write_result {
                WriteResult::Written(0) => continue,
                WriteResult::WouldBlock => {
                    self.counters.would_block += 1;
                    continue;
                }
                WriteResult::Written(written) => {
                    in_flight.offset = in_flight
                        .offset
                        .saturating_add(written)
                        .min(in_flight.samples.len());
                    if in_flight.offset == in_flight.samples.len() {
                        self.in_flight = None;
                        return Ok(AttemptResult::Completed);
                    }
                }
                WriteResult::Underrun => return Ok(AttemptResult::Underrun),
                WriteResult::Suspended => return Ok(AttemptResult::Reopen),
            }
        }
    }

    fn render_period(&mut self) -> Result<InFlight> {
        let (occupancy, limits, splice_serial, limits_serial, started, just_started) = {
            let mut shared = self.shared.lock().expect("jitter buffer mutex poisoned");
            let now = Instant::now();
            let current_index = shared.current_index(now);
            let before = shared.buffer.eviction_stats();
            let _ = shared.buffer.take(0, current_index);
            shared.refresh_splice_serial(before);
            let occupancy = shared.buffer.occupancy();
            let just_started = !shared.started && occupancy >= shared.limits.target_frames;
            if just_started {
                shared.started = true;
            }
            (
                occupancy,
                shared.limits,
                shared.splice_serial,
                shared.limits_serial,
                shared.started,
                just_started,
            )
        };

        if limits_serial != self.seen_limits_serial {
            self.seen_limits_serial = limits_serial;
            self.controller
                .reconfigure(limits.target_frames, limits.largest_packet_frames);
        }
        if splice_serial != self.seen_splice_serial {
            self.seen_splice_serial = splice_serial;
            self.mark_splice(false);
        }
        if !started {
            return Ok(self.render_silence_period());
        }
        if just_started {
            self.controller.reset_filter(occupancy);
        }

        let correction = self.controller.update(occupancy);
        if self.controller.saturation_duration() >= Duration::from_secs(5) {
            if !self.saturation_reported {
                self.timing.log_render(format_args!(
                    "drift_limit=true duration_ms={:.3} correction={correction:.6}",
                    self.controller.saturation_duration().as_secs_f64() * 1000.0
                ));
                self.saturation_reported = true;
            }
        } else {
            self.saturation_reported = false;
        }
        self.resampler.set_correction(correction)?;
        let needed = self.resampler.input_frames_next();
        let (taken, take_splice_serial) = {
            let mut shared = self.shared.lock().expect("jitter buffer mutex poisoned");
            let current_index = shared.current_index(Instant::now());
            let before = shared.buffer.eviction_stats();
            let taken = shared.buffer.take(needed, current_index);
            shared.refresh_splice_serial(before);
            (taken, shared.splice_serial)
        };

        if take_splice_serial != self.seen_splice_serial {
            self.seen_splice_serial = take_splice_serial;
            self.mark_splice(false);
            self.resampler
                .set_correction(self.controller.correction())?;
            if taken
                .as_ref()
                .is_some_and(|audio| audio.samples.len() != self.resampler.input_frames_next())
            {
                return Ok(self.render_silence_period());
            }
        }

        match taken {
            Some(taken) => self.render_source_period(taken),
            None => Ok(self.render_silence_period()),
        }
    }

    fn render_source_period(&mut self, taken: TakenAudio) -> Result<InFlight> {
        let mut output = self.resampler.process(&taken.samples)?.to_vec();
        let ramp_from = self
            .ramp_from
            .take()
            .or_else(|| self.source_was_silent.then_some(0.0));
        if let Some(from) = ramp_from {
            apply_ramp(&mut output, from, self.declick_frames());
        }
        self.last_output = output.last().copied().unwrap_or(self.last_output);
        self.source_was_silent = false;
        self.rendered_source = true;
        self.starving_for = Duration::ZERO;
        Ok(InFlight {
            samples: output.into_iter().map(f32_to_s32).collect(),
            offset: 0,
            oldest_insertion_index: Some(taken.oldest_insertion_index),
        })
    }

    fn render_silence_period(&mut self) -> InFlight {
        let parameters = self.sink.as_ref().expect("sink is open").parameters();
        let mut output = vec![0.0; parameters.period_frames];
        self.counters.silence_frames = self
            .counters
            .silence_frames
            .saturating_add(parameters.period_frames as u64);
        if !self.source_was_silent {
            self.mark_splice(false);
        }
        if let Some(from) = self.ramp_from.take() {
            apply_ramp(&mut output, from, self.declick_frames());
        }
        self.source_was_silent = true;
        self.last_output = 0.0;
        if self.rendered_source {
            self.starving_for += parameters.period();
            if self.starving_for >= self.policy.starvation_reconnect() {
                self.send_event(RendererEvent::Starvation);
                self.cancel.store(true, Ordering::Release);
            }
        }
        InFlight {
            samples: output.into_iter().map(f32_to_s32).collect(),
            offset: 0,
            oldest_insertion_index: None,
        }
    }

    fn in_flight_is_expired(&self) -> bool {
        let Some(oldest) = self
            .in_flight
            .as_ref()
            .and_then(|period| period.oldest_insertion_index)
        else {
            return false;
        };
        let shared = self.shared.lock().expect("jitter buffer mutex poisoned");
        shared.current_index(Instant::now()).saturating_sub(oldest)
            > shared.limits.maximum_age_periods
    }

    fn hard_write_recovery(&mut self) -> Result<()> {
        self.counters.hard_recoveries += 1;
        self.timing
            .log_render("sink_recovery=write_expiry action=drop_prepare");
        self.in_flight = None;
        let recovered = self
            .sink
            .as_mut()
            .expect("sink is open")
            .drop_and_prepare()
            .is_ok();
        self.clear_media_and_reset();
        self.consecutive_expiries = 0;
        if recovered {
            Ok(())
        } else {
            self.reopen_sink(false)
        }
    }

    fn recover_underrun(&mut self) -> Result<()> {
        if self.in_flight_is_expired() {
            self.in_flight = None;
            self.mark_splice(false);
            return Ok(());
        }
        let started = *self
            .underrun_recovery_started
            .get_or_insert_with(Instant::now);
        if started.elapsed() >= WRITE_ATTEMPT * WRITE_EXPIRY_ATTEMPTS {
            self.in_flight = None;
            self.clear_media_and_reset();
            return self.reopen_sink(false);
        }
        if self.sink.as_mut().expect("sink is open").prepare().is_err() {
            return self.reopen_sink(false);
        }
        self.timing
            .log_render("sink_recovery=underrun action=prepare_preserve_suffix");
        let occupancy = self.current_occupancy();
        self.resampler.reset();
        self.controller.reset_all(occupancy);
        self.ramp_from = Some(self.last_output);
        self.consecutive_expiries = 0;
        Ok(())
    }

    fn reopen_sink(&mut self, initial_open: bool) -> Result<()> {
        self.counters.reopen_attempts += 1;
        self.timing.log_render("sink_recovery=reopen started=true");
        if let Some(sink) = self.sink.as_mut() {
            let _ = sink.drop_queue();
        }
        self.sink = None;
        self.in_flight = None;

        let started = Instant::now();
        let mut backoff = REOPEN_INITIAL_BACKOFF;
        let mut next_diagnostic = 0usize;
        let mut next_periodic = Duration::from_secs(60);
        let mut repeated_config: Option<(String, u32, Instant)> = None;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            match self.factory.open(
                self.source_rate,
                self.policy.requested_alsa_buffer(),
                REQUESTED_PERIOD,
            ) {
                Ok(sink) => {
                    let parameters = sink.parameters();
                    self.reconfigure_after_reopen(parameters)?;
                    self.sink = Some(sink);
                    self.log_configuration(parameters);
                    self.timing.log_render(format_args!(
                        "sink_recovery=reopen completed=true elapsed_ms={:.3}",
                        started.elapsed().as_secs_f64() * 1000.0
                    ));
                    return Ok(());
                }
                Err(error) if error.is::<MissingPulsePlugin>() => return Err(error),
                Err(error) if error.is::<ConfigurationError>() => {
                    self.counters.reopen_failures += 1;
                    if initial_open {
                        return Err(error);
                    }
                    self.timing.log_render(format_args!(
                        "sink_recovery=reopen incompatible=true reopen_failure_count={}",
                        self.counters.reopen_failures
                    ));
                    let message = error.to_string();
                    let now = Instant::now();
                    match &mut repeated_config {
                        Some((previous, count, first)) if *previous == message => {
                            *count += 1;
                            if *count >= 3 && now.duration_since(*first) >= Duration::from_secs(5) {
                                return Err(error);
                            }
                        }
                        state => *state = Some((message, 1, now)),
                    }
                }
                Err(error) => {
                    self.counters.reopen_failures += 1;
                    repeated_config = None;
                    if elapsed_crossed_diagnostic(
                        started.elapsed(),
                        &mut next_diagnostic,
                        &mut next_periodic,
                    ) {
                        eprintln!(
                            "ALSA device unavailable for {:.1}s: {error:#}",
                            started.elapsed().as_secs_f64()
                        );
                        self.timing.log_render(format_args!(
                            "sink_recovery=reopen unavailable=true duration_ms={:.3} reopen_failure_count={}",
                            started.elapsed().as_secs_f64() * 1000.0,
                            self.counters.reopen_failures
                        ));
                    }
                }
            }
            interruptible_sleep(backoff, &self.cancel);
            backoff = (backoff * 2).min(REOPEN_MAX_BACKOFF);
        }
    }

    fn reconfigure_after_reopen(&mut self, parameters: SinkParameters) -> Result<()> {
        let ramp_from = self.rendered_source.then_some(self.last_output);
        self.resampler = checked_resampler(self.source_rate, parameters)?;
        let mut shared = self.shared.lock().expect("jitter buffer mutex poisoned");
        let limits = self.policy.derive_limits(
            self.source_rate,
            shared.largest_packet_frames,
            parameters.period(),
        )?;
        shared.period = parameters.period();
        shared.limits = limits;
        shared.limits_serial = shared.limits_serial.wrapping_add(1);
        let current_index = shared.current_index(Instant::now());
        shared.buffer.update_limits(
            limits.maximum_frames,
            limits.maximum_age_periods,
            current_index,
        );
        shared.clear_and_reanchor(Instant::now());
        self.controller = OccupancyController::new(
            self.source_rate,
            limits.target_frames,
            limits.largest_packet_frames,
            parameters.period(),
        );
        self.source_was_silent = true;
        self.last_output = 0.0;
        self.ramp_from = ramp_from;
        self.rendered_source = false;
        self.starving_for = Duration::ZERO;
        self.consecutive_expiries = 0;
        self.underrun_recovery_started = None;
        Ok(())
    }

    fn clear_media_and_reset(&mut self) {
        let ramp_from = self.rendered_source.then_some(self.last_output);
        {
            let mut shared = self.shared.lock().expect("jitter buffer mutex poisoned");
            shared.clear_and_reanchor(Instant::now());
        }
        let occupancy = self.current_occupancy();
        self.resampler.reset();
        self.controller.reset_all(occupancy);
        self.source_was_silent = true;
        self.last_output = 0.0;
        self.ramp_from = ramp_from;
        self.rendered_source = false;
        self.starving_for = Duration::ZERO;
    }

    fn log_configuration(&self, parameters: SinkParameters) {
        let shared = self.shared.lock().expect("jitter buffer mutex poisoned");
        let output_delay = self.resampler.output_delay(parameters.rate);
        eprintln!(
            "input {}Hz, output {}Hz, period {:.3}ms, ALSA buffer {:.3}ms, jitter target {:.3}ms, max {:.3}ms",
            self.source_rate,
            parameters.rate,
            parameters.period().as_secs_f64() * 1000.0,
            parameters.buffer().as_secs_f64() * 1000.0,
            shared.limits.target_frames as f64 * 1000.0 / self.source_rate as f64,
            shared.limits.maximum_frames as f64 * 1000.0 / self.source_rate as f64,
        );
        self.timing.log_render(format_args!(
            "requested_rate_hz={} actual_rate_hz={} requested_period_ms={:.3} actual_period_ms={:.3} requested_buffer_ms={:.3} actual_buffer_ms={:.3} nominal_ratio={:.9} output_delay_ms={:.3} submission_bound_ms={:.3} alsa_client_bound_ms={:.3}",
            self.source_rate,
            parameters.rate,
            REQUESTED_PERIOD.as_secs_f64() * 1000.0,
            parameters.period().as_secs_f64() * 1000.0,
            self.policy.requested_alsa_buffer().as_secs_f64() * 1000.0,
            parameters.buffer().as_secs_f64() * 1000.0,
            parameters.rate as f64 / self.source_rate as f64,
            output_delay.as_secs_f64() * 1000.0,
            shared.limits.submission_bound.as_secs_f64() * 1000.0,
            (shared.limits.submission_bound + parameters.buffer()).as_secs_f64() * 1000.0,
        ));
    }

    fn mark_splice(&mut self, reset_integral: bool) {
        self.counters.splices += 1;
        let occupancy = self.current_occupancy();
        self.resampler.reset();
        if reset_integral {
            self.controller.reset_all(occupancy);
        } else {
            self.controller.reset_filter(occupancy);
        }
        self.ramp_from = Some(self.last_output);
    }

    fn current_occupancy(&self) -> usize {
        self.shared
            .lock()
            .expect("jitter buffer mutex poisoned")
            .buffer
            .occupancy()
    }

    fn declick_frames(&self) -> usize {
        let rate = self.sink.as_ref().expect("sink is open").parameters().rate;
        (rate as u64 * DECLICK.as_millis() as u64 / 1000) as usize
    }

    fn send_event(&self, event: RendererEvent) {
        let _ = self.events.blocking_send(event);
    }

    fn log_period(&self) {
        let shared = self.shared.lock().expect("jitter buffer mutex poisoned");
        let sink = self.sink.as_ref().expect("sink is open");
        let parameters = sink.parameters();
        let delay = sink.delay_frames().unwrap_or_default();
        let evictions = shared.buffer.eviction_stats();
        self.timing.log_render(format_args!(
            "occupancy_frames={} target_frames={} maximum_frames={} correction={:.6} filtered_error_ms={:.3} saturation_ms={:.3} alsa_delay_frames={} evicted_capacity_frames={} evicted_capacity_ms={:.3} evicted_age_frames={} evicted_age_ms={:.3} generated_silence_ms={:.3} continuous_starvation_ms={:.3} period_ms={:.3} buffer_ms={:.3} would_block_count={} write_expiry_count={} underrun_count={} hard_recovery_count={} reopen_count={} reopen_failure_count={} splice_count={}",
            shared.buffer.occupancy(),
            shared.limits.target_frames,
            shared.limits.maximum_frames,
            self.controller.correction(),
            self.controller.filtered_error_seconds() * 1000.0,
            self.controller.saturation_duration().as_secs_f64() * 1000.0,
            delay,
            evictions.capacity_frames,
            evictions.capacity_frames as f64 * 1000.0 / self.source_rate as f64,
            evictions.age_frames,
            evictions.age_frames as f64 * 1000.0 / self.source_rate as f64,
            self.counters.silence_frames as f64 * 1000.0 / parameters.rate as f64,
            self.starving_for.as_secs_f64() * 1000.0,
            parameters.period().as_secs_f64() * 1000.0,
            parameters.buffer().as_secs_f64() * 1000.0,
            self.counters.would_block,
            self.counters.write_expiries,
            self.counters.underruns,
            self.counters.hard_recoveries,
            self.counters.reopen_attempts,
            self.counters.reopen_failures,
            self.counters.splices,
        ));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptResult {
    Completed,
    Expired,
    Underrun,
    Reopen,
}

fn checked_resampler(source_rate: u32, parameters: SinkParameters) -> Result<FixedOutputResampler> {
    let resampler =
        FixedOutputResampler::new(source_rate, parameters.rate, parameters.period_frames)?;
    let delay = resampler.output_delay(parameters.rate);
    if delay > MAX_GROUP_DELAY {
        return Err(ConfigurationError::new(format!(
            "Rubato output delay {:.3} ms exceeds the measured {:.3} ms provenance-free limit",
            delay.as_secs_f64() * 1000.0,
            MAX_GROUP_DELAY.as_secs_f64() * 1000.0,
        ))
        .into());
    }
    Ok(resampler)
}

pub fn period_index(elapsed: Duration, period: Duration) -> u64 {
    if period.is_zero() {
        return 0;
    }
    (elapsed.as_nanos() / period.as_nanos()).min(u64::MAX as u128) as u64
}

fn apply_ramp(samples: &mut [f32], from: f32, frames: usize) {
    let count = frames.min(samples.len());
    for (index, sample) in samples.iter_mut().take(count).enumerate() {
        let position = (index + 1) as f32 / count.max(1) as f32;
        *sample = from + (*sample - from) * position;
    }
}

fn f32_to_s32(sample: f32) -> i32 {
    (sample.clamp(-1.0, 1.0) * i32::MAX as f32) as i32
}

fn interruptible_sleep(duration: Duration, cancel: &AtomicBool) {
    let deadline = Instant::now() + duration;
    while !cancel.load(Ordering::Acquire) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

fn elapsed_crossed_diagnostic(
    elapsed: Duration,
    next_fixed: &mut usize,
    next_periodic: &mut Duration,
) -> bool {
    if *next_fixed < REOPEN_DIAGNOSTIC_TIMES.len()
        && elapsed >= REOPEN_DIAGNOSTIC_TIMES[*next_fixed]
    {
        *next_fixed += 1;
        return true;
    }
    if elapsed >= *next_periodic {
        *next_periodic += Duration::from_secs(60);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeState {
        waits: VecDeque<WaitResult>,
        availability: VecDeque<Availability>,
        writes: VecDeque<WriteResult>,
        prepare_calls: usize,
        drop_prepare_calls: usize,
        drop_calls: usize,
        written_frames: usize,
        submitted_lengths: Vec<usize>,
        open_failures: usize,
        incompatible_open: bool,
    }

    struct FakeSink {
        parameters: SinkParameters,
        state: Arc<Mutex<FakeState>>,
    }

    impl AudioSink for FakeSink {
        fn parameters(&self) -> SinkParameters {
            self.parameters
        }

        fn wait_ready(&self, _timeout: Duration) -> Result<WaitResult> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .waits
                .pop_front()
                .unwrap_or(WaitResult::Ready))
        }

        fn available_frames(&self) -> Result<Availability> {
            Ok(self
                .state
                .lock()
                .unwrap()
                .availability
                .pop_front()
                .unwrap_or(Availability::Frames(self.parameters.period_frames)))
        }

        fn write(&mut self, samples: &[i32]) -> Result<WriteResult> {
            let mut state = self.state.lock().unwrap();
            state.submitted_lengths.push(samples.len());
            let result = state
                .writes
                .pop_front()
                .unwrap_or(WriteResult::Written(samples.len()));
            if let WriteResult::Written(frames) = result {
                state.written_frames += frames;
            }
            Ok(result)
        }

        fn prepare(&mut self) -> Result<()> {
            self.state.lock().unwrap().prepare_calls += 1;
            Ok(())
        }

        fn drop_and_prepare(&mut self) -> Result<()> {
            self.state.lock().unwrap().drop_prepare_calls += 1;
            Ok(())
        }

        fn drop_queue(&mut self) -> Result<()> {
            self.state.lock().unwrap().drop_calls += 1;
            Ok(())
        }

        fn delay_frames(&self) -> Result<i64> {
            Ok(0)
        }
    }

    struct FakeFactory {
        parameters: SinkParameters,
        state: Arc<Mutex<FakeState>>,
    }

    impl SinkFactory for FakeFactory {
        fn open(
            &self,
            _requested_rate: u32,
            _requested_buffer: Duration,
            _requested_period: Duration,
        ) -> Result<Box<dyn AudioSink>> {
            {
                let mut state = self.state.lock().unwrap();
                if state.incompatible_open {
                    return Err(ConfigurationError::new("incompatible fake PCM").into());
                }
                if state.open_failures > 0 {
                    state.open_failures -= 1;
                    anyhow::bail!("fake PCM unavailable");
                }
            }
            Ok(Box::new(FakeSink {
                parameters: self.parameters,
                state: Arc::clone(&self.state),
            }))
        }
    }

    fn test_worker() -> (
        RenderWorker,
        Arc<Mutex<FakeState>>,
        mpsc::Receiver<RendererEvent>,
    ) {
        let parameters = SinkParameters {
            rate: 48_000,
            period_frames: 480,
            buffer_frames: 1920,
        };
        let policy = PlayPolicy {
            target_buffer_ms: Some(50),
            default_target_buffer_ms: 50,
            maximum_buffer_ms: Some(100),
            alsa_buffer_ms: 40,
            starvation_reconnect_ms: 1500,
        };
        let limits = policy
            .derive_limits(48_000, 1024, parameters.period())
            .unwrap();
        let shared = Arc::new(Mutex::new(SharedAudio {
            buffer: JitterBuffer::new(limits.maximum_frames, limits.maximum_age_periods),
            limits,
            source_rate: 48_000,
            largest_packet_frames: 1024,
            period: parameters.period(),
            age_epoch: Instant::now(),
            started: false,
            splice_serial: 0,
            limits_serial: 0,
        }));
        let state = Arc::new(Mutex::new(FakeState::default()));
        let sink = Box::new(FakeSink {
            parameters,
            state: Arc::clone(&state),
        });
        let factory: Arc<dyn SinkFactory> = Arc::new(FakeFactory {
            parameters,
            state: Arc::clone(&state),
        });
        let (events, receiver) = mpsc::channel(4);
        let worker = RenderWorker::new(
            Some(sink),
            factory,
            policy,
            48_000,
            shared,
            Arc::new(AtomicBool::new(false)),
            events,
            checked_resampler(48_000, parameters).unwrap(),
            OccupancyController::new(48_000, limits.target_frames, 1024, parameters.period()),
            TimingDebug::from_env(),
        );
        (worker, state, receiver)
    }

    #[test]
    fn period_index_is_quantized_monotonic_time() {
        let period = Duration::from_millis(10);
        assert_eq!(period_index(Duration::from_millis(9), period), 0);
        assert_eq!(period_index(Duration::from_millis(10), period), 1);
        assert_eq!(period_index(Duration::from_millis(25), period), 2);
    }

    #[test]
    fn declick_ramp_reaches_new_signal() {
        let mut samples = vec![1.0; 4];
        apply_ramp(&mut samples, 0.0, 4);
        assert_eq!(samples, vec![0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn readiness_expiry_does_not_consume_jitter_media() {
        let (mut worker, state, _receiver) = test_worker();
        worker
            .shared
            .lock()
            .unwrap()
            .buffer
            .insert(vec![0.25; 3000], 0);
        state.lock().unwrap().waits.push_back(WaitResult::TimedOut);

        assert_eq!(worker.write_one_attempt().unwrap(), AttemptResult::Expired);
        assert_eq!(worker.current_occupancy(), 3000);
        assert!(worker.in_flight.is_none());
    }

    #[test]
    fn readiness_requires_a_complete_output_period() {
        let (mut worker, state, _receiver) = test_worker();
        worker
            .shared
            .lock()
            .unwrap()
            .buffer
            .insert(vec![0.25; 3000], 0);
        let mut state = state.lock().unwrap();
        state.availability.push_back(Availability::Frames(479));
        state.waits.push_back(WaitResult::Ready);
        state.waits.push_back(WaitResult::TimedOut);
        drop(state);

        assert_eq!(worker.write_one_attempt().unwrap(), AttemptResult::Expired);
        assert_eq!(worker.current_occupancy(), 3000);
        assert!(worker.in_flight.is_none());
    }

    #[test]
    fn readiness_underrun_is_not_misclassified_as_reopen() {
        let (mut worker, state, _receiver) = test_worker();
        state.lock().unwrap().waits.push_back(WaitResult::Underrun);
        assert_eq!(worker.write_one_attempt().unwrap(), AttemptResult::Underrun);
    }

    #[test]
    fn soft_expiry_preserves_an_existing_in_flight_period() {
        let (mut worker, state, _receiver) = test_worker();
        worker.in_flight = Some(InFlight {
            samples: vec![1; 480],
            offset: 17,
            oldest_insertion_index: None,
        });
        state.lock().unwrap().waits.push_back(WaitResult::TimedOut);

        assert_eq!(worker.write_one_attempt().unwrap(), AttemptResult::Expired);
        assert_eq!(worker.in_flight.as_ref().unwrap().offset, 17);
    }

    #[test]
    fn hard_write_recovery_clears_media_and_drops_pcm_queue() {
        let (mut worker, state, _receiver) = test_worker();
        worker.rendered_source = true;
        worker.last_output = 0.25;
        worker
            .shared
            .lock()
            .unwrap()
            .buffer
            .insert(vec![0.25; 3000], 0);
        worker.in_flight = Some(InFlight {
            samples: vec![1; 480],
            offset: 0,
            oldest_insertion_index: Some(0),
        });

        worker.hard_write_recovery().unwrap();
        assert_eq!(worker.current_occupancy(), 0);
        assert!(worker.in_flight.is_none());
        assert_eq!(worker.ramp_from, Some(0.25));
        assert_eq!(state.lock().unwrap().drop_prepare_calls, 1);
    }

    #[test]
    fn underrun_preserves_fresh_buffer_and_suffix() {
        let (mut worker, state, _receiver) = test_worker();
        worker
            .shared
            .lock()
            .unwrap()
            .buffer
            .insert(vec![0.25; 3000], 0);
        worker.in_flight = Some(InFlight {
            samples: vec![1; 480],
            offset: 37,
            oldest_insertion_index: Some(0),
        });

        worker.recover_underrun().unwrap();
        assert_eq!(worker.current_occupancy(), 3000);
        assert_eq!(worker.in_flight.as_ref().unwrap().offset, 37);
        assert_eq!(state.lock().unwrap().prepare_calls, 1);
    }

    #[test]
    fn indexed_expiry_wins_over_underrun_suffix_preservation() {
        let (mut worker, state, _receiver) = test_worker();
        {
            let mut shared = worker.shared.lock().unwrap();
            shared.limits.maximum_age_periods = 0;
            shared.age_epoch = Instant::now() - Duration::from_millis(20);
        }
        worker.in_flight = Some(InFlight {
            samples: vec![1; 480],
            offset: 37,
            oldest_insertion_index: Some(0),
        });

        worker.recover_underrun().unwrap();
        assert!(worker.in_flight.is_none());
        assert_eq!(state.lock().unwrap().prepare_calls, 0);
    }

    #[test]
    fn partial_writes_submit_only_the_unwritten_suffix() {
        let (mut worker, state, _receiver) = test_worker();
        worker.in_flight = Some(InFlight {
            samples: vec![1; 480],
            offset: 0,
            oldest_insertion_index: None,
        });
        state
            .lock()
            .unwrap()
            .writes
            .extend([WriteResult::Written(17), WriteResult::Written(463)]);

        assert_eq!(
            worker.write_one_attempt().unwrap(),
            AttemptResult::Completed
        );
        let state = state.lock().unwrap();
        assert_eq!(state.submitted_lengths, vec![480, 463]);
        assert_eq!(state.written_frames, 480);
    }

    #[test]
    fn starvation_emits_one_reconnect_event_at_the_threshold() {
        let (mut worker, _state, mut receiver) = test_worker();
        worker.rendered_source = true;

        for _ in 0..149 {
            let _ = worker.render_silence_period();
            assert!(receiver.try_recv().is_err());
        }
        let _ = worker.render_silence_period();
        assert!(matches!(receiver.try_recv(), Ok(RendererEvent::Starvation)));
        assert!(worker.cancel.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn initial_pcm_unavailability_retries_locally_until_open() {
        let (mut worker, state, _receiver) = test_worker();
        worker.sink = None;
        state.lock().unwrap().open_failures = 1;

        worker.reopen_sink(true).unwrap();

        assert!(worker.sink.is_some());
        assert_eq!(worker.counters.reopen_attempts, 1);
        assert_eq!(worker.counters.reopen_failures, 1);
    }

    #[test]
    fn incompatible_initial_pcm_is_immediately_terminal() {
        let (mut worker, state, _receiver) = test_worker();
        worker.sink = None;
        state.lock().unwrap().incompatible_open = true;

        let error = worker.reopen_sink(true).unwrap_err();

        assert!(error.is::<ConfigurationError>());
        assert_eq!(worker.counters.reopen_failures, 1);
    }
}

//! How long things take, and whether the audio thread is keeping up.
//!
//! Two different questions, measured two different ways.
//!
//! **The callback's deadline.** For a real-time thread the number that matters
//! is not milliseconds but the fraction of the buffer deadline that was used:
//! `processing_time / (buffer_frames / sample_rate)`. At 48 kHz and 512 frames
//! the deadline is 10.7 ms, and a callback that averages 10 % of it is
//! comfortable right up to the buffer where it takes 105 %, which is a dropout.
//! [`Callback`] keeps that fraction's mean, its peak and a count of buffers that
//! went over — a **software xrun counter**, and the one number to ask a user for
//! when they report that it crackles.
//!
//! This half is always on. Two `Instant::now()` calls and four relaxed atomic
//! stores per buffer is forty nanoseconds against a ten millisecond deadline,
//! and instrumentation that has to be switched on before it can be asked for is
//! instrumentation that is not there when the question is asked.
//!
//! **Everything else.** The scheduler's per-bar planning, the interface's
//! per-frame draw, import, export and start-up are not real-time and are not
//! measured against a deadline; what is useful there is a distribution, and a
//! distribution costs a mutex and a `VecDeque` push per call. So those are
//! [`Scope`]s, and they are off unless `CHORD_TOOL_TIMING` is set. Instrumentation
//! that is always on is instrumentation that eventually costs more than it
//! explains.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// -----------------------------------------------------------------------------
// The callback's deadline
// -----------------------------------------------------------------------------

/// What one turn of the audio callback cost, against the time it had.
///
/// Shared between the callback, which writes it, and the log thread, which reads
/// it. Nothing here takes a lock: the callback cannot wait for a reader, so every
/// field is a relaxed atomic and the summary is allowed to be a few buffers out
/// of date.
#[derive(Debug, Default)]
pub struct Callback {
    buffers: AtomicU64,
    overruns: AtomicU64,
    /// Total time the callback was busy, in nanoseconds.
    busy_nanos: AtomicU64,
    /// Total time the callback had, in nanoseconds.
    deadline_nanos: AtomicU64,
    /// The most recent buffer's load, as `f32` bits.
    last: AtomicU32,
    /// The worst load so far, as `f32` bits.
    peak: AtomicU32,
}

impl Callback {
    pub const fn new() -> Self {
        Callback {
            buffers: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            busy_nanos: AtomicU64::new(0),
            deadline_nanos: AtomicU64::new(0),
            last: AtomicU32::new(0),
            peak: AtomicU32::new(0),
        }
    }

    /// A counter to hand to an engine that is not behind the output tap.
    pub fn shared() -> Arc<Self> {
        Arc::new(Callback::new())
    }

    /// Record one buffer: how long it took, and how long it had.
    ///
    /// Called from the audio thread, so: no locks, no allocation, and relaxed
    /// ordering throughout. A reader that sees the count without the total is
    /// reading a report from a moment ago, which is the only kind of report a
    /// real-time thread can give.
    pub fn record(&self, used: Duration, deadline: Duration) {
        let used_nanos = used.as_nanos() as u64;
        // A zero-length buffer has no deadline; one nanosecond keeps the ratio
        // finite rather than either a division by zero or an infinity in the
        // log.
        let deadline_nanos = (deadline.as_nanos() as u64).max(1);

        self.buffers.fetch_add(1, Ordering::Relaxed);
        self.busy_nanos.fetch_add(used_nanos, Ordering::Relaxed);
        self.deadline_nanos
            .fetch_add(deadline_nanos, Ordering::Relaxed);
        if used_nanos >= deadline_nanos {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }

        let load = used_nanos as f32 / deadline_nanos as f32;
        self.last.store(load.to_bits(), Ordering::Relaxed);
        // The peak is the one field that has to be read-modify-write. A
        // compare-exchange rather than a load-then-store, because two callbacks
        // can overlap on a machine with more than one audio thread, and the
        // loser of that race would silently drop the worst buffer of the two.
        let mut seen = f32::from_bits(self.peak.load(Ordering::Relaxed));
        while load > seen {
            match self.peak.compare_exchange_weak(
                seen.to_bits(),
                load.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => seen = f32::from_bits(actual),
            }
        }
    }

    pub fn buffers(&self) -> u64 {
        self.buffers.load(Ordering::Relaxed)
    }

    /// How many buffers missed their deadline.
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// The fraction of the deadline the last buffer used.
    pub fn last_load(&self) -> f32 {
        f32::from_bits(self.last.load(Ordering::Relaxed))
    }

    /// The fraction of the deadline the worst buffer used.
    pub fn peak_load(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    /// The fraction of the deadline the average buffer used.
    ///
    /// Averaged over all of them rather than over a window: this is reset by
    /// restarting the program, and a rolling mean would need a second buffer of
    /// history the callback would have to maintain.
    pub fn mean_load(&self) -> f32 {
        let deadline = self.deadline_nanos.load(Ordering::Relaxed);
        if deadline == 0 {
            0.0
        } else {
            self.busy_nanos.load(Ordering::Relaxed) as f32 / deadline as f32
        }
    }

    /// One line, as it goes into `debug.log`.
    pub fn summary(&self) -> String {
        format!(
            "callbacks {}, load {:.1}% last / {:.1}% mean / {:.1}% peak, {} over deadline",
            self.buffers(),
            self.last_load() * 100.0,
            self.mean_load() * 100.0,
            self.peak_load() * 100.0,
            self.overruns(),
        )
    }
}

// -----------------------------------------------------------------------------
// Named scopes
// -----------------------------------------------------------------------------

/// Whether the named scopes are recording.
///
/// Read once per scope rather than once per drop, because a scope is opened on a
/// hot path and an environment lookup is not: the answer cannot change while the
/// program is running, and the first caller pays for the `OnceLock`.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CHORD_TOOL_TIMING").is_some_and(|v| !v.is_empty()))
}

/// How many recent samples a scope keeps for its percentiles.
const RECENT: usize = 512;

/// One named thing being timed.
#[derive(Debug, Default, Clone)]
struct Stats {
    count: u64,
    total: Duration,
    longest: Duration,
    /// The most recent samples, oldest first. Only these are sorted for the
    /// percentiles, so a scope that has been running for an hour costs the same
    /// as one that has been running for a second.
    recent: VecDeque<Duration>,
}

impl Stats {
    fn add(&mut self, took: Duration) {
        self.count += 1;
        self.total += took;
        self.longest = self.longest.max(took);
        if self.recent.len() == RECENT {
            self.recent.pop_front();
        }
        self.recent.push_back(took);
    }

    fn percentile(&self, percent: usize) -> Duration {
        if self.recent.is_empty() {
            return Duration::ZERO;
        }
        let mut sorted: Vec<Duration> = self.recent.iter().copied().collect();
        sorted.sort_unstable();
        // The nearest-rank definition, which never interpolates between two
        // samples and so never reports a time that did not happen.
        let rank = (percent * sorted.len()).div_ceil(100).max(1);
        sorted[rank.min(sorted.len()) - 1]
    }

    fn line(&self, name: &str) -> String {
        let mean = if self.count == 0 {
            Duration::ZERO
        } else {
            self.total / self.count as u32
        };
        format!(
            "{} {}x mean {} p50 {} p99 {} max {}",
            name,
            self.count,
            millis(mean),
            millis(self.percentile(50)),
            millis(self.percentile(99)),
            millis(self.longest),
        )
    }
}

fn millis(d: Duration) -> String {
    format!("{:.2}ms", d.as_secs_f64() * 1000.0)
}

static SCOPES: Mutex<Vec<(&'static str, Stats)>> = Mutex::new(Vec::new());

fn record(name: &'static str, took: Duration) {
    // A poisoned lock is a panic in some other thread, not a reason to bring
    // down the audio interface; the timing is dropped rather than propagated.
    let Ok(mut scopes) = SCOPES.lock() else {
        return;
    };
    match scopes.iter_mut().find(|(n, _)| *n == name) {
        Some((_, stats)) => stats.add(took),
        None => {
            let mut stats = Stats::default();
            stats.add(took);
            scopes.push((name, stats));
        }
    }
}

/// Times one call, and adds it to the named scope when it is dropped.
///
/// ```ignore
/// let _scope = Scope::new("export");
/// ```
///
/// Held in a variable rather than written as `let _ = Scope::new(…)`, which
/// would drop it on the same line and time nothing.
#[must_use = "a scope times the span it is alive for; bind it to a name"]
pub struct Scope {
    name: &'static str,
    started: Instant,
    recording: bool,
}

impl Scope {
    pub fn new(name: &'static str) -> Self {
        Scope {
            name,
            started: Instant::now(),
            recording: enabled(),
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.recording {
            record(self.name, self.started.elapsed());
        }
    }
}

/// Every scope's line, worst p99 first, and clear the recent windows.
///
/// The totals are cumulative for the life of the process; the percentiles cover
/// the interval since the last call, which is what the log thread calls once a
/// second. A scope that has spent most of its life idle therefore reports the
/// last second rather than its whole history, which is the reading you want when
/// you are watching a number change.
pub fn take_scopes() -> Vec<String> {
    let Ok(mut scopes) = SCOPES.lock() else {
        return Vec::new();
    };
    let mut lines: Vec<(Duration, String)> = scopes
        .iter_mut()
        .filter(|(_, stats)| !stats.recent.is_empty())
        .map(|(name, stats)| {
            let line = stats.line(name);
            let rank = stats.percentile(99);
            stats.recent.clear();
            (rank, line)
        })
        .collect();
    lines.sort_by(|a, b| b.0.cmp(&a.0));
    lines.into_iter().map(|(_, line)| line).collect()
}

/// The whole timing line for `debug.log`, or `None` when nothing is recording.
///
/// The callback's counters are always available; the scopes only exist when
/// `CHORD_TOOL_TIMING` is set.
pub fn report(callback: &Callback) -> Option<String> {
    let scopes = take_scopes();
    if scopes.is_empty() && callback.buffers() == 0 {
        return None;
    }
    let mut line = callback.summary();
    if !scopes.is_empty() {
        line.push_str(" | ");
        line.push_str(&scopes.join(" | "));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_inside_its_deadline_is_not_an_overrun() {
        let callback = Callback::new();
        callback.record(Duration::from_millis(1), Duration::from_millis(10));
        assert_eq!(callback.buffers(), 1);
        assert_eq!(callback.overruns(), 0);
        assert!((callback.last_load() - 0.1).abs() < 1e-6);
        assert!((callback.mean_load() - 0.1).abs() < 1e-6);
        assert!((callback.peak_load() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn a_buffer_that_takes_its_whole_deadline_is_an_overrun() {
        // At the deadline, not merely past it: a callback that finishes exactly
        // as the next buffer is wanted has missed it.
        let callback = Callback::new();
        callback.record(Duration::from_millis(10), Duration::from_millis(10));
        assert_eq!(callback.overruns(), 1);
        assert!((callback.last_load() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_peak_keeps_the_worst_buffer_and_the_mean_keeps_them_all() {
        let callback = Callback::new();
        callback.record(Duration::from_millis(1), Duration::from_millis(10));
        callback.record(Duration::from_millis(9), Duration::from_millis(10));
        callback.record(Duration::from_millis(2), Duration::from_millis(10));
        assert_eq!(callback.buffers(), 3);
        assert_eq!(callback.overruns(), 0);
        assert!((callback.mean_load() - 0.4).abs() < 1e-6);
        assert!((callback.peak_load() - 0.9).abs() < 1e-6);
        assert!((callback.last_load() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_zero_length_buffer_does_not_divide_by_zero() {
        // A device that asks for nothing cannot be late, so the deadline is
        // floored at a nanosecond rather than the ratio being allowed to become
        // a division by zero or an infinity in the log.
        let callback = Callback::new();
        callback.record(Duration::ZERO, Duration::ZERO);
        assert_eq!(callback.buffers(), 1);
        assert_eq!(callback.overruns(), 0);
        assert_eq!(callback.last_load(), 0.0);
        assert!(callback.mean_load().is_finite());
        assert!(callback.summary().contains("0.0% last"));
    }

    #[test]
    fn the_summary_says_what_the_counters_say() {
        let callback = Callback::new();
        callback.record(Duration::from_millis(1), Duration::from_millis(10));
        callback.record(Duration::from_millis(20), Duration::from_millis(10));
        let line = callback.summary();
        assert!(line.contains("callbacks 2"), "{line}");
        assert!(line.contains("1 over deadline"), "{line}");
        assert!(line.contains("200.0% peak"), "{line}");
    }

    #[test]
    fn the_percentiles_are_times_that_happened() {
        let mut stats = Stats::default();
        for ms in 1..=100u64 {
            stats.add(Duration::from_micros(ms * 100));
        }
        assert_eq!(stats.count, 100);
        assert_eq!(stats.longest, Duration::from_micros(10_000));
        // Nearest rank: the 50th of a hundred one-to-ten-millisecond samples is
        // the five-millisecond one, not an interpolation between two of them.
        assert_eq!(stats.percentile(50), Duration::from_micros(5_000));
        assert_eq!(stats.percentile(99), Duration::from_micros(9_900));
        assert_eq!(stats.percentile(100), Duration::from_micros(10_000));
    }

    #[test]
    fn the_recent_window_does_not_grow_without_bound() {
        let mut stats = Stats::default();
        for ms in 1..=(RECENT as u64 * 4) {
            stats.add(Duration::from_micros(ms));
        }
        assert_eq!(stats.count, RECENT as u64 * 4);
        assert_eq!(stats.recent.len(), RECENT);
        // And the window holds the *last* ones.
        assert_eq!(
            *stats.recent.back().unwrap(),
            Duration::from_micros(RECENT as u64 * 4)
        );
    }

    #[test]
    fn a_zero_count_scope_reports_a_zero_mean_rather_than_panicking() {
        let stats = Stats::default();
        assert!(stats.line("empty").contains("0x mean 0.00ms"));
    }

    #[test]
    fn the_report_line_carries_the_callback_and_the_scopes() {
        // `record` rather than a `Scope`, because the scopes are off unless the
        // environment says otherwise and a test cannot set one for the whole
        // process without racing every other test in the binary.
        //
        // The registry is process-wide by design — there is one program — so
        // every assertion here is a `contains` rather than an equality, and the
        // windows are drained before the measurement.
        let _ = take_scopes();

        let callback = Callback::new();
        callback.record(Duration::from_millis(2), Duration::from_millis(10));
        record("test.scope.reported", Duration::from_millis(3));

        let line = report(&callback).expect("a line");
        assert!(line.contains("callbacks 1"), "{line}");
        assert!(line.contains("20.0% last"), "{line}");
        assert!(line.contains("test.scope.reported 1x"), "{line}");

        // The percentiles describe the interval since the last report, so the
        // next line has the callback again and no scopes of its own.
        let next = report(&callback).expect("a line");
        assert!(next.contains("callbacks 1"), "{next}");
        assert!(!next.contains("test.scope.reported"), "{next}");
    }

    #[test]
    fn recording_under_a_name_accumulates_under_that_name() {
        // Every scope name in the program is a distinct string literal, so two
        // scopes sharing one is a copy-paste in the call sites and not a runtime
        // possibility worth defending against.
        record("test.scope.accumulates", Duration::from_millis(2));
        record("test.scope.accumulates", Duration::from_millis(4));
        let scopes = SCOPES.lock().unwrap();
        let (_, stats) = scopes
            .iter()
            .find(|(n, _)| *n == "test.scope.accumulates")
            .expect("the scope was recorded");
        assert_eq!(stats.count, 2);
        assert_eq!(stats.total, Duration::from_millis(6));
        assert_eq!(stats.longest, Duration::from_millis(4));
    }
}

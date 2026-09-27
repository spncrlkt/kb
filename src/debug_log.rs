//! Debug logging: a single file shared by the input thread and the audio
//! output tap. Every line is prefixed with a wall-clock timestamp and a
//! `[IN]`, `[OUT]` or `[TIME]` tag so the streams can be read together or split
//! with grep.
//!
//! Input is flushed immediately (rare, durable). Output is flushed roughly
//! once per second (frequent, buffered by the OS anyway).
//!
//! `[TIME]` is the line to ask for when the sound is crackling: it carries the
//! callback's share of its buffer deadline, its worst buffer and how many
//! buffers missed, plus — when `CHORD_TOOL_TIMING` is set — the last second's
//! distribution for the scheduler, the draw loop, import, export and start-up.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::timing::Callback;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub struct Logger {
    file: Mutex<File>,
}

impl Logger {
    pub fn create(path: &str) -> io::Result<Arc<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Ok(Arc::new(Logger {
            file: Mutex::new(file),
        }))
    }

    /// Log an input event. Flushes immediately.
    pub fn input(&self, msg: &str) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{:.3} [IN]  {}", now(), msg);
            let _ = f.flush();
        }
    }

    /// Log an output level. Called from the output tap thread at 120 Hz;
    /// does not flush, since the OS will buffer writes.
    fn output(&self, level: u8) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{:.3} [OUT] {:03}", now(), level);
        }
    }

    /// Log a timing line. Called from the output tap thread once a second.
    pub(crate) fn timing(&self, msg: &str) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{:.3} [TIME] {}", now(), msg);
        }
    }

    /// Flush any buffered output.
    pub fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

pub struct OutputTap {
    peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    timing: Arc<Callback>,
    handle: Option<JoinHandle<()>>,
}

impl OutputTap {
    pub fn create(logger: Arc<Logger>) -> Self {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let timing = Callback::shared();

        let p = peak.clone();
        let s = stop.clone();
        let t = timing.clone();
        let handle = thread::spawn(move || {
            let interval = Duration::from_micros(1_000_000 / 60);
            let mut tick: u32 = 0;
            while !s.load(Ordering::Relaxed) {
                let bits = p.swap(0, Ordering::Relaxed);
                let v = f32::from_bits(bits);
                let level = (v.clamp(0.0, 1.0) * 255.0) as u8;
                logger.output(level);

                tick = tick.wrapping_add(1);
                if tick.is_multiple_of(60) {
                    // Once a second, alongside the output level: the timing is
                    // read here rather than written per buffer, so the audio
                    // thread pays for two `Instant`s and four relaxed stores and
                    // nothing else.
                    if let Some(line) = crate::timing::report(&t) {
                        logger.timing(&line);
                    }
                    logger.flush();
                }

                thread::sleep(interval);
            }
            logger.flush();
        });

        OutputTap {
            peak,
            stop,
            timing,
            handle: Some(handle),
        }
    }

    /// The atomic to hand to the audio callback.
    pub fn peak(&self) -> Arc<AtomicU32> {
        self.peak.clone()
    }

    /// The counters to hand to the engine.
    pub fn timing(&self) -> Arc<Callback> {
        self.timing.clone()
    }
}

impl Drop for OutputTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timing_line_is_tagged_so_it_can_be_grepped_apart_from_the_rest() {
        let path = std::env::temp_dir().join(format!(
            "chord-tool-debug-log-{}-{:?}.log",
            std::process::id(),
            std::thread::current().id()
        ));
        let logger = Logger::create(path.to_str().unwrap()).unwrap();
        logger.timing("callbacks 100, load 3.1% last / 4.0% mean / 9.0% peak, 0 over deadline");
        logger.flush();

        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.contains(" [TIME] "), "{text}");
        assert!(text.contains("0 over deadline"), "{text}");
        // And the tag is distinct from the two the log already had, so
        // `grep '\[TIME\]'` is the whole timing history and nothing else.
        assert!(!text.contains("[IN]") && !text.contains("[OUT]"), "{text}");
    }

    #[test]
    fn the_output_tap_writes_the_timing_line_once_a_second() {
        // The whole point of the counter is that a user reporting a crackle can
        // be asked for one line out of `debug.log`, so the wiring from the
        // engine's counters to that line is worth a second of test time. The tap
        // ticks at 60 Hz and reports every sixtieth.
        let path = std::env::temp_dir().join(format!(
            "chord-tool-tap-{}-{:?}.log",
            std::process::id(),
            std::thread::current().id()
        ));
        let logger = Logger::create(path.to_str().unwrap()).unwrap();
        let tap = OutputTap::create(logger.clone());

        // As the engine would: a buffer that took a fifth of its deadline.
        tap.timing()
            .record(Duration::from_millis(2), Duration::from_millis(10));

        let give_up = std::time::Instant::now() + Duration::from_millis(2_000);
        let mut text;
        loop {
            logger.flush();
            text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("[TIME]") || std::time::Instant::now() > give_up {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        drop(tap);
        let _ = std::fs::remove_file(&path);

        assert!(text.contains("[TIME]"), "no timing line in:\n{text}");
        assert!(text.contains("callbacks 1"), "{text}");
        assert!(text.contains("20.0% last"), "{text}");
        // The level tap is still running alongside it.
        assert!(text.contains("[OUT]"), "{text}");
    }
}

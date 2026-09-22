//! Debug logging: a single file shared by the input thread and the audio
//! output tap. Every line is prefixed with a wall-clock timestamp and a
//! `[IN]` or `[OUT]` tag so the two streams can be read together or split
//! with grep.
//!
//! Input is flushed immediately (rare, durable). Output is flushed roughly
//! once per second (frequent, buffered by the OS anyway).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    handle: Option<JoinHandle<()>>,
}

impl OutputTap {
    pub fn create(logger: Arc<Logger>) -> Self {
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let p = peak.clone();
        let s = stop.clone();
        let handle = thread::spawn(move || {
            let interval = Duration::from_micros(1_000_000 / 60);
            let mut tick: u32 = 0;
            while !s.load(Ordering::Relaxed) {
                let bits = p.swap(0, Ordering::Relaxed);
                let v = f32::from_bits(bits);
                let level = (v.clamp(0.0, 1.0) * 255.0) as u8;
                logger.output(level);

                tick = tick.wrapping_add(1);
                if tick % 60 == 0 {
                    logger.flush();
                }

                thread::sleep(interval);
            }
            logger.flush();
        });

        OutputTap {
            peak,
            stop,
            handle: Some(handle),
        }
    }

    /// The atomic to hand to the audio callback.
    pub fn peak(&self) -> Arc<AtomicU32> {
        self.peak.clone()
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

//! A live spectrum: thirteen band levels per tap, one tap per register plus the
//! mix.
//!
//! This is the readout that answers "what is actually there", which is the one
//! thing the equaliser cannot tell you. It runs **inside the audio callback**, on
//! the four signals the ear gets:
//!
//! | Tap | Where it is taken |
//! | --- | --- |
//! | `low`, `mid`, `high` | after that register's own curve and its fader, before the pan — so a meter reads the part's contribution to the mix |
//! | `master` | the finished pair, after the master curve, the master gain and the soft clip — so a meter reads what leaves the device |
//!
//! # A filter bank, not a transform
//!
//! Thirteen bandpass biquads in parallel per tap, each with an envelope
//! follower. That is a deliberate choice over an FFT:
//!
//! - the bands are **the equaliser's own ladder**, so a level and the curve that
//!   is shaping it are read on the same thirteen columns, which is the whole
//!   point of having both;
//! - the resolution is semi-low *by construction* rather than by computing a
//!   transform and throwing most of it away;
//! - it needs no dependency, no window function, no input buffer and no
//!   allocation — the callback touches 52 biquads and 52 followers and nothing
//!   else;
//! - the cost is a fixed number of multiplies per sample rather than a function
//!   of a transform size.
//!
//! The filter is [`crate::eq::Section::bandpass`] — the same code the equaliser's
//! bells use, so there is one biquad in the crate rather than two that drift.
//!
//! # Ballistics
//!
//! Instant attack, exponential release, which is the standard meter. Attack has
//! to be instant or a transient is missed; the release is what makes a display
//! readable, and it is adjustable because a fast release shows a *rhythm* and a
//! slow one shows a *balance*.
//!
//! # Peaks are held by the panel, not here
//!
//! The callback publishes levels and nothing else. The panel reads all of them at
//! frame rate and keeps its own peaks, which is one less piece of state on the
//! audio thread and one fewer way for a reset to be missed.

use crate::eq::{BiquadState, Section, EQ_BANDS};

/// One readout per register plus the mix, in `low`/`mid`/`high`/`master` order —
/// the same order as [`crate::eq::EqTarget`], whose target row this shares.
pub const TAPS: usize = 4;

/// The release rates the `speed` row offers, in dB per second, slowest last.
///
/// How fast a level falls is what makes a display readable: a slow release
/// integrates and shows the balance of a part, a fast one shows its rhythm.
/// Above roughly 60 dB/s the bar stops integrating at all and flickers to the
/// waveform, which is why the fastest here is 48.
pub const SPEEDS: [(f32, &str); 3] = [(48.0, "fast"), (24.0, "medium"), (8.0, "slow")];

/// Which of [`SPEEDS`] a fresh panel starts on.
pub const DEFAULT_SPEED: usize = 1;

/// How wide each band is, in the usual `Q` sense.
///
/// Chosen against the ladder rather than in the abstract. At `Q = 2` a tone a
/// full octave from a band's centre reads about ten decibels down in it, and one
/// two thirds of an octave away — the nearest neighbour at the bottom of the
/// ladder — about seven. That is a visible hump either side of the column the
/// tone is really in, which is what a display this coarse should look like; a
/// wider filter smears a single tone across three columns at four decibels each
/// and stops meaning anything.
const BAND_Q: f32 = 2.0;

/// Below this an envelope is treated as silence.
///
/// Not zero at the comparison: the recursion multiplies by a factor below one, so
/// it approaches zero without reaching it, and a band nobody is playing would
/// otherwise decay for ever through the denormals — which on some hardware costs
/// far more than the filter it is decaying out of.
const FLOOR: f32 = 1.0e-7;

/// One band: a filter, its memory, and how loud it has been recently.
#[derive(Clone)]
struct Band {
    section: Section,
    state: BiquadState,
    env: f32,
}

/// The whole bank: one set of thirteen filters per tap.
///
/// Built once on the main thread before the stream starts, so nothing here
/// allocates inside the callback.
pub struct Analyzer {
    bands: Vec<Band>,
    /// Release as a per-sample factor, redesigned when the speed row moves.
    release: f32,
    speed: f32,
}

impl Analyzer {
    /// A bank for a stream running at `sample_rate`.
    pub fn new(sample_rate: f32) -> Self {
        let mut bands = Vec::with_capacity(TAPS * EQ_BANDS);
        for _ in 0..TAPS {
            for band in 0..EQ_BANDS {
                bands.push(Band {
                    section: Section::bandpass(sample_rate, crate::eq::BANDS[band], BAND_Q),
                    state: BiquadState::default(),
                    env: 0.0,
                });
            }
        }
        let speed = SPEEDS[DEFAULT_SPEED].0;
        Analyzer {
            bands,
            release: release_factor(speed, sample_rate),
            speed,
        }
    }

    /// Empty every bank: the envelopes and the filters behind them.
    ///
    /// Called when the readout is switched back on. The banks are not run while
    /// nobody is looking at the panel, so without this the first frame would draw
    /// whatever level was on screen when it was last closed — a lie about a sound
    /// that has moved on in the meantime. Clearing the biquad state as well as the
    /// envelopes matters because the filters hold a tail: a re-enabled bank would
    /// otherwise ring out a note that finished while it was asleep.
    pub fn clear(&mut self) {
        for band in &mut self.bands {
            band.state = BiquadState::default();
            band.env = 0.0;
        }
    }

    /// Point the ballistics at a release rate in dB per second.
    ///
    /// Called once per audio buffer, and does nothing at all unless the row has
    /// moved — so the `powf` behind it is not a per-buffer cost.
    pub fn set_speed(&mut self, db_per_sec: f32, sample_rate: f32) {
        // `f32::clamp` passes a NaN straight through, and a NaN release would
        // turn every envelope in the bank into a NaN that never recovers.
        let db_per_sec = if db_per_sec.is_finite() {
            db_per_sec.clamp(1.0, 200.0)
        } else {
            SPEEDS[DEFAULT_SPEED].0
        };
        if self.speed == db_per_sec {
            return;
        }
        self.speed = db_per_sec;
        self.release = release_factor(db_per_sec, sample_rate);
    }

    /// Feed one sample to one tap, and to that tap's whole bank.
    pub fn tick(&mut self, tap: usize, x: f32) {
        let start = tap * EQ_BANDS;
        let Some(bank) = self.bands.get_mut(start..start + EQ_BANDS) else {
            return;
        };
        for band in bank {
            let y = band.section.tick(&mut band.state, x).abs();
            // Everything below the floor is silence, exactly — including the
            // filter's own tail. A bandpass ringing down through the denormals
            // produces values far below it for ever, and without this the attack
            // branch below would keep picking them up and the envelope would
            // never reach zero. Denormals are also slow on some hardware, which
            // is the second reason to leave them behind.
            let y = if y > FLOOR { y } else { 0.0 };
            // Instant attack, exponential release. Release is the common path, so
            // a band that is already quiet costs a compare and nothing else until
            // it reaches the floor.
            band.env = if y > band.env {
                y
            } else if band.env > FLOOR {
                band.env * self.release
            } else {
                0.0
            };
        }
    }

    /// The thirteen levels of one tap, in band order.
    ///
    /// What the callback publishes: a slice-ish walk with no per-band indexing
    /// arithmetic at the call site, and an empty iterator rather than a panic if
    /// the tap is out of range.
    pub fn levels(&self, tap: usize) -> impl Iterator<Item = f32> + '_ {
        let start = tap * EQ_BANDS;
        self.bands
            .get(start..start + EQ_BANDS)
            .unwrap_or(&[])
            .iter()
            .map(|b| b.env)
    }

    /// One band's level, for the tests and for anything that wants a single
    /// reading. The callback publishes by walking [`Analyzer::levels`] instead.
    #[cfg(test)]
    pub fn level(&self, tap: usize, band: usize) -> f32 {
        self.bands
            .get(tap * EQ_BANDS + band)
            .map(|b| b.env)
            .unwrap_or(0.0)
    }
}

/// The per-sample release factor for a rate in dB per second.
fn release_factor(db_per_sec: f32, sample_rate: f32) -> f32 {
    10.0f32.powf(-db_per_sec / (20.0 * sample_rate.max(1.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eq::BANDS as BAND_HZ;

    const SR: f32 = 44_100.0;

    /// A steady sine into one tap, long enough for the filters to ring up.
    fn feed(analyzer: &mut Analyzer, tap: usize, freq: f32, samples: usize) {
        for i in 0..samples {
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / SR).sin();
            analyzer.tick(tap, x);
        }
    }

    fn db(level: f32) -> f32 {
        20.0 * level.max(1.0e-9).log10()
    }

    #[test]
    fn a_full_scale_sine_reads_full_scale_in_its_own_band() {
        // The property the constant-peak-gain bandpass exists for: a full-scale
        // tone reads as full scale wherever it lands, so a reading is a level
        // rather than a level multiplied by a bandwidth nobody chose.
        for (band, freq) in [(0usize, 20.0f32), (7, 1000.0), (12, 16_000.0)] {
            let mut analyzer = Analyzer::new(SR);
            feed(&mut analyzer, 0, freq, 20_000);
            let level = analyzer.level(0, band);
            assert!(
                db(level).abs() < 1.0,
                "{} Hz in band {} read {} dB",
                freq,
                band,
                db(level)
            );
        }
    }

    #[test]
    fn half_scale_reads_six_decibels_down() {
        // A meter has to be linear in between its ends, not merely correct at
        // them.
        let mut analyzer = Analyzer::new(SR);
        for i in 0..20_000 {
            let x = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR).sin();
            analyzer.tick(0, x);
        }
        let level = db(analyzer.level(0, 7));
        assert!((level + 6.02).abs() < 1.0, "half scale read {} dB", level);
    }

    #[test]
    fn a_tone_stays_in_its_own_band() {
        // A 1 kHz sine belongs on the 1 kHz column and nowhere near its
        // neighbours. A display that leaks an octave is telling you something
        // that is not there.
        let mut analyzer = Analyzer::new(SR);
        feed(&mut analyzer, 0, 1000.0, 20_000);
        let own = db(analyzer.level(0, 7));
        assert!((own).abs() < 1.0, "the tone itself: {} dB", own);
        // The nearest neighbours are a full octave away, where this `Q` puts
        // them about ten decibels down.
        for band in [5usize, 6, 8, 9, 11] {
            let leaked = db(analyzer.level(0, band));
            assert!(
                leaked < own - 7.0,
                "band {} leaked {} dB against {}",
                band,
                leaked,
                own
            );
        }
    }

    #[test]
    fn every_band_answers_its_own_centre() {
        // Thirteen filters designed from one ladder: if any were wired to the
        // wrong centre, the display would have a dead or a doubled column.
        for (band, freq) in BAND_HZ.iter().enumerate() {
            let mut analyzer = Analyzer::new(SR);
            feed(&mut analyzer, 0, *freq, 20_000);
            let level = db(analyzer.level(0, band));
            assert!(
                level.abs() < 1.5,
                "band {} ({} Hz) read {} dB",
                band,
                freq,
                level
            );
        }
    }

    #[test]
    fn clear_empties_the_envelopes_and_the_filters_behind_them() {
        // Both halves matter. An envelope left standing is a level from a sound
        // that has stopped; a filter left ringing is a tail that the next frame
        // would read as if it had just been played. The readout is switched off
        // while the panel is away, so coming back has to start from nothing.
        let mut analyzer = Analyzer::new(SR);
        feed(&mut analyzer, 1, 1000.0, 4_000);
        assert!(analyzer.level(1, 7) > 0.5, "the tap should have rung up");

        analyzer.clear();

        for tap in 0..TAPS {
            for band in 0..EQ_BANDS {
                assert_eq!(analyzer.level(tap, band), 0.0, "tap {tap} band {band}");
            }
        }

        // And the filters really are empty: one sample of silence through a
        // ringing bank would otherwise read whatever tail was in the state.
        analyzer.tick(1, 0.0);
        assert_eq!(analyzer.level(1, 7), 0.0);
    }

    #[test]
    fn the_taps_do_not_talk_to_each_other() {
        // Four banks in one flat vector: an off-by-one in the slice arithmetic
        // would make the master meter show the low register.
        let mut analyzer = Analyzer::new(SR);
        feed(&mut analyzer, 2, 1000.0, 20_000);
        assert!(analyzer.level(2, 7) > 0.5, "the fed tap should read");
        for tap in [0usize, 1, 3] {
            for band in 0..EQ_BANDS {
                assert_eq!(
                    analyzer.level(tap, band),
                    0.0,
                    "tap {} band {} heard tap 2",
                    tap,
                    band
                );
            }
        }
        // And `levels` agrees with `level`, including for a tap off the end.
        assert_eq!(analyzer.levels(2).count(), EQ_BANDS);
        assert_eq!(analyzer.levels(9).count(), 0);
    }

    #[test]
    fn silence_decays_and_reaches_the_floor_exactly() {
        let mut analyzer = Analyzer::new(SR);
        analyzer.set_speed(SPEEDS[0].0, SR);
        feed(&mut analyzer, 0, 1000.0, 20_000);
        let loud = analyzer.level(0, 7);
        assert!(loud > 0.5);

        // At the fastest speed, a hundred milliseconds should already be
        // visible, and five seconds is a hundred and fifty times the full range.
        for _ in 0..4_410 {
            analyzer.tick(0, 0.0);
        }
        let shortly = analyzer.level(0, 7);
        assert!(shortly < loud, "{} against {}", shortly, loud);

        for _ in 0..220_500 {
            analyzer.tick(0, 0.0);
        }
        assert_eq!(analyzer.level(0, 7), 0.0, "it must reach the floor");
    }

    #[test]
    fn a_faster_speed_falls_further_in_the_same_time() {
        let mut fall = Vec::new();
        for (rate, name) in SPEEDS {
            let mut analyzer = Analyzer::new(SR);
            analyzer.set_speed(rate, SR);
            feed(&mut analyzer, 0, 1000.0, 20_000);
            let loud = analyzer.level(0, 7);
            for _ in 0..20_000 {
                analyzer.tick(0, 0.0);
            }
            fall.push((name, db(analyzer.level(0, 7)) - db(loud)));
        }
        assert!(
            fall[0].1 < fall[1].1 && fall[1].1 < fall[2].1,
            "fast/medium/slow fell {:?}",
            fall
        );
    }

    #[test]
    fn the_release_is_the_rate_that_was_asked_for() {
        // A factor below one is not a rate. This checks the arithmetic, because
        // "24 dB per second" is what the panel promises and nothing else would
        // notice if it were 24 dB per buffer.
        for (rate, name) in SPEEDS {
            let factor = release_factor(rate, SR);
            let per_second = SR as f64 * (factor as f64).log10();
            let measured = -20.0 * per_second;
            assert!(
                (measured - rate as f64).abs() < 0.01,
                "{} ({}) came out as {} dB/s",
                rate,
                name,
                measured
            );
        }
    }

    #[test]
    fn an_absurd_speed_is_clamped_rather_than_becoming_a_gain() {
        // A release factor above one would grow instead of falling. The panel
        // cannot ask for it, but a hand-edited file or a later row can.
        let mut analyzer = Analyzer::new(SR);
        analyzer.set_speed(100_000.0, SR);
        assert!(analyzer.release <= 1.0, "release {}", analyzer.release);
        analyzer.set_speed(0.0, SR);
        assert!(analyzer.release <= 1.0 && analyzer.release > 0.0);
        analyzer.set_speed(f32::NAN, SR);
        assert!(analyzer.release > 0.0 && analyzer.release <= 1.0);
    }

    #[test]
    fn every_band_stays_finite_at_every_extreme() {
        // White noise at full scale into all four taps at once: the worst case,
        // and the one that would show a bandpass pole sitting on the unit
        // circle.
        let mut seed = 0x9e37_79b9u32;
        let mut analyzer = Analyzer::new(SR);
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
            for tap in 0..TAPS {
                analyzer.tick(tap, x);
            }
        }
        for tap in 0..TAPS {
            for band in 0..EQ_BANDS {
                let level = analyzer.level(tap, band);
                assert!(level.is_finite(), "tap {} band {}", tap, band);
                assert!(level < 2.0, "tap {} band {} at {}", tap, band, level);
            }
        }
    }

    #[test]
    fn a_low_rate_device_keeps_every_band_finite() {
        // At 32 kHz the 16 kHz band is far above the corner clamp, which is what
        // stops its poles landing on Nyquist. The equaliser has the same guard;
        // this is the analyser's half of it.
        let mut seed = 7u32;
        let mut analyzer = Analyzer::new(32_000.0);
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = (seed >> 8) as f32 / 8_388_608.0 - 1.0;
            analyzer.tick(0, x);
        }
        for band in 0..EQ_BANDS {
            let level = analyzer.level(0, band);
            assert!(
                level.is_finite() && level < 2.0,
                "band {} at {}",
                band,
                level
            );
        }
    }
}

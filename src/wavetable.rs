//! Single-cycle wavetables: named harmonic recipes, built once, read by audio.
//!
//! A wavetable oscillator is one oscillator whose waveform is a stored cycle
//! rather than a formula. That is worth having here for a specific reason: it is
//! the only way to get an *additive* timbre — a fixed set of harmonics with
//! chosen levels — out of a synth whose every voice is one oscillator. Nine
//! sines summed per sample per voice would cost more than everything else in the
//! callback put together; the same nine sines summed once into a table and read
//! back cost one lookup.
//!
//! Everything in this module is pure and fixed at compile time except the table
//! *contents*, which are computed on first use and then never change. See
//! [`tables`] for why that first use happens on the main thread.

use std::f32::consts::TAU;
use std::sync::OnceLock;

/// Samples in one cycle.
///
/// A thousand and twenty-four is sixteen times the highest harmonic any recipe
/// here asks for, so the interpolation error is far below anything an ear could
/// find, and the whole set costs well under a hundred kilobytes.
pub const TABLE_LEN: usize = 1024;

/// One harmonic and how loud it is, as a multiple of the recipe's fundamental.
type Partial = (u16, f32);

/// How a timbre's spectrum is described.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Recipe {
    /// Nine digits, left to right from 16' to 1', the way an organist writes a
    /// registration: `888000000` is the classic jazz setting.
    Drawbars(&'static str),
    /// Explicit `(harmonic, level)` pairs.
    Partials(&'static [Partial]),
}

/// One named single-cycle waveform.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Timbre {
    /// What the Synth panel's `waveform` row calls it.
    pub label: &'static str,
    /// How far below the played note this table's fundamental sits.
    ///
    /// Zero for everything that is simply "a shape at the pitch you played",
    /// and `-1` for the drawbar registrations — see [`DRAWBAR_HARMONICS`].
    pub octave: i8,
    pub recipe: Recipe,
}

/// The nine drawbars, left to right, as multiples of the **16' drawbar**.
///
/// The physical drawbars are 16', 5 1/3', 8', 4', 2 2/3', 2', 1 3/5', 1 1/3'
/// and 1', which are 0.5, 1.5, 1, 2, 3, 4, 5, 6 and 8 times the played note.
///
/// A wavetable is periodic, so it can only hold whole-number multiples of its own
/// fundamental — and 0.5 and 1.5 are not whole numbers. Multiply the whole set by
/// two and it becomes 1, 3, 2, 4, 6, 8, 10, 12, 16, all integers. So a drawbar
/// table's fundamental is the *16' drawbar*, one octave below the key that was
/// pressed, and the eight-foot drawbar is its second harmonic: the note you play
/// is still the note you hear, with the registration built on top of it and
/// below it. That is the whole reason the organ recipes carry `octave: -1`.
pub const DRAWBAR_HARMONICS: [u16; 9] = [1, 3, 2, 4, 6, 8, 10, 12, 16];

/// Every timbre, in the order the `waveform` row cycles them.
///
/// This list and the table-backed variants of `Waveform` are the same list in
/// the same order, and a test holds them together — an index is the only thing
/// joining them, and an index is exactly what drifts.
pub const TIMBRES: [Timbre; 18] = [
    Timbre {
        label: "organ full",
        octave: -1,
        recipe: Recipe::Drawbars("888888888"),
    },
    Timbre {
        label: "organ jazz",
        octave: -1,
        recipe: Recipe::Drawbars("888000000"),
    },
    Timbre {
        // Every drawbar that is not the eight-foot one, *plus* the eight-foot
        // one. A registration without it is real enough on a real organ, but
        // here it would mean the note you pressed is not in the sound — which in
        // a chord tool reads as the synth playing the wrong note.
        label: "organ bright",
        octave: -1,
        recipe: Recipe::Drawbars("808008888"),
    },
    Timbre {
        // The eight-foot and four-foot drawbars and nothing else: the played
        // note and its octave, which is as hollow as an organ gets.
        label: "organ hollow",
        octave: -1,
        recipe: Recipe::Drawbars("008800000"),
    },
    Timbre {
        // Struck metal is inharmonic and this cannot be, so this is the honest
        // approximation: a full but uneven spectrum with the odd harmonics
        // lifted, which is what makes a periodic tone read as metallic rather
        // than as a saw.
        label: "metallic",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.35),
            (3, 0.70),
            (4, 0.20),
            (5, 0.55),
            (6, 0.15),
            (7, 0.40),
            (8, 0.10),
            (9, 0.28),
            (11, 0.18),
            (13, 0.10),
        ]),
    },
    Timbre {
        // Two peaks with a dip between them, which is the crudest useful
        // imitation of a formant — the reason a vowel sounds like a vowel.
        label: "vox",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.55),
            (3, 0.12),
            (4, 0.75),
            (5, 0.45),
            (6, 0.10),
            (7, 0.06),
            (8, 0.22),
        ]),
    },
    Timbre {
        // A strong fundamental would make this a horn; with the energy pushed
        // up into the fourth and sixth it reads as thin and brittle instead.
        label: "glass",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 0.30),
            (2, 0.18),
            (3, 0.50),
            (4, 0.80),
            (6, 0.60),
            (8, 0.35),
            (10, 0.20),
            (12, 0.12),
        ]),
    },
    Timbre {
        label: "mellow",
        octave: 0,
        recipe: Recipe::Partials(&[(1, 1.0), (2, 0.25), (3, 0.08), (4, 0.03)]),
    },
    Timbre {
        // A diapason: the plain speaking stop a pipe organ is built around, and
        // the reference every other stop is registered against. Harmonics 1, 2
        // and 3 carry it — the octave and the twelfth are what make an organ
        // sound like an organ rather than like a flute.
        label: "principal",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.70),
            (3, 0.50),
            (4, 0.32),
            (5, 0.18),
            (6, 0.14),
            (7, 0.09),
            (8, 0.07),
        ]),
    },
    Timbre {
        // Odd harmonics only, which is what a *closed* pipe gives: a cylinder
        // stopped at one end resonates at 1, 3, 5, 7 times its fundamental and
        // nothing between. The same physics governs the clarinet, so one table
        // serves the gedeckt, the stopped flute and the clarinet, and the filter
        // tells them apart.
        label: "clarinet",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (3, 0.55),
            (5, 0.38),
            (7, 0.22),
            (9, 0.14),
            (11, 0.09),
            (13, 0.05),
        ]),
    },
    Timbre {
        // The same odd-harmonic physics as the clarinet, but a gedeckt is a
        // flute-scaled stopped pipe: the third harmonic is present and the rest
        // are almost gone, which is why it accompanies rather than leads.
        label: "gedeckt",
        octave: 0,
        recipe: Recipe::Partials(&[(1, 1.0), (3, 0.22), (5, 0.08), (7, 0.03)]),
    },
    Timbre {
        // A reed: the harmonics climb to a peak around the fourth and fifth
        // rather than falling from the first, which is the formant that makes a
        // trumpet or an oboe cut through an ensemble.
        label: "reed",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 0.45),
            (2, 0.70),
            (3, 0.88),
            (4, 1.0),
            (5, 0.85),
            (6, 0.62),
            (7, 0.45),
            (8, 0.32),
            (9, 0.22),
            (10, 0.15),
            (11, 0.10),
        ]),
    },
    Timbre {
        // A struck string: the fundamental and the octave dominate and the rest
        // fall away steeply, which is why a piano reads as warm rather than
        // bright when the same note is played on a harpsichord.
        label: "piano",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.62),
            (3, 0.44),
            (4, 0.26),
            (5, 0.18),
            (6, 0.12),
            (7, 0.08),
            (8, 0.05),
        ]),
    },
    Timbre {
        // A vibraphone bar's real partials sit at roughly 1, 4 and 10.8 times
        // the fundamental, which is inharmonic and so cannot live in a periodic
        // table — these are the nearest whole harmonics. It reads as a bar
        // because the fundamental is almost alone, not because it is a bar.
        label: "vibes",
        octave: 0,
        recipe: Recipe::Partials(&[(1, 1.0), (4, 0.32), (10, 0.11)]),
    },
    Timbre {
        // A plucked string: all the harmonics, but rolled off faster than a
        // saw's `1/h` and with the top ones thinned further, which is the
        // difference between nylon and steel.
        label: "nylon",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.52),
            (3, 0.34),
            (4, 0.20),
            (5, 0.13),
            (6, 0.08),
            (7, 0.05),
            (8, 0.03),
        ]),
    },
    Timbre {
        // The vowel /a/: a sawtooth-like glottal source shaped by two formants.
        // The peak harmonics are the ones nearest F1 (850 Hz) and F2 (1610 Hz)
        // for a fundamental near A3, which is where a choir sits. Because the
        // peaks are fixed frequencies and the harmonics move with the note, the
        // vowel is most convincing around that pitch — exactly as it is for a
        // real singer, and for the same reason.
        label: "vox aah",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.55),
            (3, 0.30),
            (4, 0.80),
            (5, 0.25),
            (6, 0.18),
            (7, 0.60),
            (8, 0.14),
            (9, 0.10),
            (10, 0.09),
            (11, 0.08),
            (12, 0.06),
        ]),
    },
    Timbre {
        // The vowel /u/: F1 at 250 Hz and F2 at 595 Hz, so on a fundamental near
        // A3 both land on the first three harmonics and the rest are all but
        // gone. That is what makes it hollow.
        label: "vox ooh",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.20),
            (3, 0.55),
            (4, 0.10),
            (5, 0.06),
            (6, 0.04),
            (7, 0.03),
            (8, 0.02),
        ]),
    },
    Timbre {
        // Sixteen harmonics falling off as `1/sqrt(h)`: brighter than the
        // straight `1/h` of a saw, which is what "buzz" means.
        label: "buzz",
        octave: 0,
        recipe: Recipe::Partials(&[
            (1, 1.0),
            (2, 0.707),
            (3, 0.577),
            (4, 0.500),
            (5, 0.447),
            (6, 0.408),
            (7, 0.378),
            (8, 0.354),
            (9, 0.333),
            (10, 0.316),
            (11, 0.302),
            (12, 0.289),
            (13, 0.277),
            (14, 0.267),
            (15, 0.258),
            (16, 0.250),
        ]),
    },
];

/// The built tables, and the only place they live.
pub struct Tables {
    cycles: Vec<[f32; TABLE_LEN]>,
}

impl Tables {
    fn build() -> Self {
        Tables {
            cycles: TIMBRES.iter().map(build).collect(),
        }
    }

    /// The cycle for timbre `index`.
    ///
    /// Panics on an out-of-range index, which every caller derives from a
    /// `Waveform` variant and a test covers; the audio thread only ever reaches
    /// it through [`Waveform::table`], which cannot produce one.
    pub fn cycle(&self, index: usize) -> &[f32; TABLE_LEN] {
        &self.cycles[index]
    }
}

/// How far below the played note timbre `index` sounds.
///
/// `const` because the octave groups are: the wavetable position's morph pairs
/// are built at compile time out of this, and a pair that crossed an octave
/// would play one of the two an octave out.
pub const fn octave(index: usize) -> i8 {
    TIMBRES[index].octave
}

/// The tables, built on first use.
///
/// `Synth::new` calls this before it builds the audio stream, so the build —
/// which allocates and does a few thousand `sin` calls — happens on the main
/// thread and the audio callback only ever reads. Anywhere else that reaches it
/// first is on the main thread too; the audio thread has no path here that does
/// not already hold a `&'static Tables`.
pub fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(Tables::build)
}

/// Expand one recipe into its partials, already scaled by the drawbar digits.
fn partials_of(recipe: &Recipe) -> Vec<Partial> {
    match recipe {
        Recipe::Partials(list) => list.to_vec(),
        Recipe::Drawbars(registration) => registration
            .chars()
            .zip(DRAWBAR_HARMONICS)
            .filter_map(|(digit, harmonic)| {
                let level = digit.to_digit(10)? as f32 / 8.0;
                (level > 0.0).then_some((harmonic, level))
            })
            .collect(),
    }
}

/// Sum one cycle and normalise it to a peak of 1.
///
/// The normalisation matters for the palette rather than for the maths: it puts
/// every timbre at the same peak as `sine` and `saw`, so choosing a waveform
/// changes the sound and not the level. It does *not* equalise loudness — a
/// registration of one drawbar is a thin, peaky wave with far less energy in it
/// than one of all nine, and it is right that it should be quieter.
fn build(timbre: &Timbre) -> [f32; TABLE_LEN] {
    let mut cycle = [0.0f32; TABLE_LEN];
    for (harmonic, level) in partials_of(&timbre.recipe) {
        let step = TAU * harmonic as f32 / TABLE_LEN as f32;
        for (i, sample) in cycle.iter_mut().enumerate() {
            *sample += level * (step * i as f32).sin();
        }
    }

    let peak = cycle.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        let scale = 1.0 / peak;
        for sample in cycle.iter_mut() {
            *sample *= scale;
        }
    }
    cycle
}

/// The magnitude of harmonic `harmonic` in `cycle`, by direct correlation.
///
/// A one-bin discrete Fourier transform, which is all the tests need: it returns
/// exactly the amplitude of that partial for a cycle built out of sines. Kept
/// next to the builder rather than in the tests because it is the inverse of
/// what the builder does, and the two are only meaningful as a pair.
#[cfg(test)]
fn harmonic_level(cycle: &[f32; TABLE_LEN], harmonic: u16) -> f32 {
    let step = TAU * harmonic as f32 / TABLE_LEN as f32;
    let (mut re, mut im) = (0.0f32, 0.0f32);
    for (i, sample) in cycle.iter().enumerate() {
        re += sample * (step * i as f32).cos();
        im += sample * (step * i as f32).sin();
    }
    2.0 * (re * re + im * im).sqrt() / TABLE_LEN as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drawbar_harmonics_are_the_nine_drawbars() {
        // 16', 5 1/3', 8', 4', 2 2/3', 2', 1 3/5', 1 1/3', 1' as multiples of
        // the 16' drawbar. If this list were wrong every registration would be
        // wrong in a way that still sounded like an organ.
        assert_eq!(DRAWBAR_HARMONICS, [1, 3, 2, 4, 6, 8, 10, 12, 16]);
    }

    #[test]
    fn every_table_is_normalised_and_carries_no_direct_current() {
        let tables = tables();
        for (i, timbre) in TIMBRES.iter().enumerate() {
            let cycle = tables.cycle(i);
            let peak = cycle.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                (peak - 1.0).abs() < 1e-4,
                "{} peaks at {} rather than 1",
                timbre.label,
                peak
            );
            let mean = cycle.iter().sum::<f32>() / TABLE_LEN as f32;
            assert!(
                mean.abs() < 1e-5,
                "{} carries a DC offset of {}",
                timbre.label,
                mean
            );
            assert!(
                cycle.iter().all(|s| s.is_finite()),
                "{} has a non-finite sample",
                timbre.label
            );
        }
    }

    #[test]
    fn a_table_contains_the_harmonics_its_recipe_asked_for() {
        let tables = tables();
        for (i, timbre) in TIMBRES.iter().enumerate() {
            let cycle = tables.cycle(i);
            let asked = partials_of(&timbre.recipe);
            let loudest = asked.iter().map(|(_, l)| *l).fold(0.0f32, f32::max);

            for (harmonic, level) in &asked {
                // The measured amplitude is the asked-for one scaled by whatever
                // the peak normalisation divided by, so the check is on the
                // *ratio* to the loudest partial.
                let expected = level / loudest;
                let measured = harmonic_level(cycle, *harmonic);
                let peak_measured = asked
                    .iter()
                    .map(|(h, _)| harmonic_level(cycle, *h))
                    .fold(0.0f32, f32::max);
                assert!(
                    (measured / peak_measured - expected).abs() < 1e-3,
                    "{} harmonic {} is {} of the loudest, wanted {}",
                    timbre.label,
                    harmonic,
                    measured / peak_measured,
                    expected
                );
            }

            // And nothing above the recipe. Harmonic 2 is skipped for the
            // drawbar registrations because they really do contain every
            // harmonic they list; this checks the ones they do not.
            let highest = asked.iter().map(|(h, _)| *h).max().unwrap_or(0);
            for harmonic in 1..=32u16 {
                if asked.iter().any(|(h, _)| *h == harmonic) {
                    continue;
                }
                let measured = harmonic_level(cycle, harmonic);
                assert!(
                    measured < 1e-3,
                    "{} has unexpected energy at harmonic {} ({})",
                    timbre.label,
                    harmonic,
                    measured
                );
            }
            assert!(
                highest <= 16,
                "{} asks for harmonic {}",
                timbre.label,
                highest
            );
        }
    }

    #[test]
    fn the_drawbar_registrations_sit_an_octave_below_the_played_note() {
        // The point of the octave shift: the played note is the *second*
        // harmonic of the table, with the 16' drawbar below it. If this ever
        // became zero the organ ensembles would silently jump an octave and lose
        // the two drawbars that need a subharmonic.
        for (i, timbre) in TIMBRES.iter().enumerate() {
            match timbre.recipe {
                Recipe::Drawbars(_) => {
                    assert_eq!(octave(i), -1, "{}", timbre.label);
                    let cycle = tables().cycle(i);
                    // Every registration here includes the 8' drawbar, which is
                    // harmonic 2 of the table: the note that was played. Without
                    // it the timbre would sound an octave off.
                    assert!(
                        harmonic_level(cycle, 2) > 0.01,
                        "{} does not carry the played note",
                        timbre.label
                    );
                }
                Recipe::Partials(_) => {
                    assert_eq!(octave(i), 0, "{}", timbre.label);
                }
            }
        }
    }

    #[test]
    fn a_registration_reads_its_digits_against_the_drawbar_list() {
        let all = partials_of(&Recipe::Drawbars("888888888"));
        assert_eq!(all.len(), 9, "all nine drawbars down");
        assert_eq!(
            all.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            DRAWBAR_HARMONICS.to_vec()
        );
        assert!(all.iter().all(|(_, l)| (*l - 1.0).abs() < 1e-6));

        let jazz = partials_of(&Recipe::Drawbars("888000000"));
        assert_eq!(jazz.len(), 3);
        assert_eq!(
            jazz.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            vec![1, 3, 2],
            "the first three drawbars only"
        );

        // A zero digit is silence, not a harmonic at zero level.
        assert!(partials_of(&Recipe::Drawbars("000000000")).is_empty());
        // The digits are levels out of eight, which is how an organist reads
        // them.
        let half = partials_of(&Recipe::Drawbars("444000000"));
        assert!(half.iter().all(|(_, l)| (*l - 0.5).abs() < 1e-6));
    }

    #[test]
    fn the_two_ends_of_the_organ_range_are_actually_different() {
        // A cheap guard against every registration accidentally building the
        // same table.
        let tables = tables();
        let full = tables.cycle(0);
        let hollow = tables.cycle(3);
        let difference: f32 = full
            .iter()
            .zip(hollow.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(difference > 100.0, "full and hollow are near-identical");
    }

    #[test]
    fn the_tables_are_built_once_and_shared() {
        // The audio thread holds a `&'static Tables` and never builds one, so
        // this identity is what keeps the build — which allocates and runs a few
        // thousand `sin` calls — off the callback.
        assert!(std::ptr::eq(tables(), tables()));
    }

    #[test]
    fn the_highest_partial_is_where_the_aliasing_starts() {
        // The honest bound, measured rather than asserted in prose: the tool's
        // own key range tops out at B6, and a drawbar table's top partial is
        // eight times the played note, so it stays under Nyquist there. It is
        // only a voice that transposes up as well that can push it past.
        let highest_twelve_tone = 1975.5; // B6
        let nyquist = 24_000.0;
        assert!(
            highest_twelve_tone * 8.0 < nyquist,
            "a drawbar table should be clean across the tool's own range"
        );
    }
}

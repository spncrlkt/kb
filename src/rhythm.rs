//! Rhythm patterns: a one-bar step grid, the takes stacked on it, and the
//! tapping maths that turns performance into grid.
//!
//! A pattern is a grid of `steps_per_bar` cells. Each **layer** is one take —
//! one pass of tapping — with its own gain, so "overlapping takes at decreasing
//! volume" is data rather than a playback trick, and a saved pattern sounds the
//! way it did when it was built.
//!
//! Everything here is pure: no audio, no terminal, no filesystem. `arrangement`
//! turns patterns into timed stabs, and `rhythm_store` is the only code in this
//! feature that touches disk.

use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::music::BAR_TICKS;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Grid resolutions a pattern may use, in steps per 4/4 bar.
///
/// 2 = half notes, 4 = quarter notes, 8 = eighths, 16 = sixteenths,
/// 32 = thirty-seconds, 64 = sixty-fourths. 12 and 24 are the **triplet**
/// grids — eighth-note and sixteenth-note triplets — which is what a shuffle,
/// a 12/8 blues or a swung jazz line needs; without them every feel this tool
/// could express was straight. Every one divides [`BAR_TICKS`] exactly
/// (12 → 320 ticks, 24 → 160), so a step is always a whole number of ticks, and
/// the list is in playing order: coarsest first, each triplet right after the
/// straight grid it subdivides.
pub const VALID_STEPS: [usize; 8] = [2, 4, 8, 12, 16, 24, 32, 64];

/// The grid a new pattern starts on.
pub const DEFAULT_STEPS: usize = 16;

/// The note lengths any duration snaps to, smallest first, up to a whole note.
///
/// This is the ladder the Sinko panel's `offset`, `hold` and `mute` rows move
/// along, so a press lands on a note length rather than on an arbitrary tick
/// count. The order is the one the player asked for — a 32nd, a 16th, an 8th, a
/// 3/16, a quarter — continued to the half, the dotted half and the whole,
/// because a hold has to reach those and an offset may be a whole note.
pub const NOTE_LADDER: [u32; 9] = [120, 240, 480, 720, 960, 1440, 1920, 2880, 3840];

/// How long a hit holds when nothing says otherwise: a sixteenth.
pub const DEFAULT_HOLD: u32 = 240;

/// The longest bar tail a pattern may mute: a quarter note.
pub const MAX_MUTE_TICKS: u32 = 960;

/// The rungs a muted tail may take: nothing, then every note length up to the
/// quarter the mute is capped at.
pub const MUTE_LADDER: [u32; 6] = [0, 120, 240, 480, 720, 960];

/// The offset ladder: the note lengths on either side of no offset at all.
pub fn signed_ladder() -> Vec<i32> {
    let mut rungs: Vec<i32> = NOTE_LADDER
        .iter()
        .rev()
        .map(|ticks| -(*ticks as i32))
        .collect();
    rungs.push(0);
    rungs.extend(NOTE_LADDER.iter().map(|ticks| *ticks as i32));
    rungs
}

/// Move along a ladder of values.
///
/// A value already on the ladder steps one rung. A value *off* it — anything
/// hand-edited into `rhythms.toml` — snaps to the nearest rung instead of
/// jumping past it, which is what makes the first press feel like a snap. At
/// either end the ladder clamps.
pub fn rung_step(rungs: &[i32], current: i32, delta: i32) -> i32 {
    if rungs.is_empty() {
        return current;
    }
    if let Some(index) = rungs.iter().position(|rung| *rung == current) {
        let next = (index as i32 + delta).clamp(0, rungs.len() as i32 - 1);
        return rungs[next as usize];
    }
    *rungs
        .iter()
        .min_by_key(|rung| (**rung - current).abs())
        .unwrap_or(&current)
}

/// A hold in ticks from a fraction of one grid cell.
///
/// The constructors below take a gate because "half a cell" is how a staccato
/// hit is thought about, and how the built-ins were described; the pattern itself
/// stores ticks, so a hold no longer moves when the grid does.
fn hold_from_gate(gate: f32, steps: usize) -> u32 {
    let steps = steps.max(1);
    let step_ticks = BAR_TICKS / steps as u64;
    let ticks = (step_ticks as f64 * gate.clamp(0.05, steps as f32) as f64).round();
    (ticks as u64).clamp(1, BAR_TICKS) as u32
}

/// Gains never decay below this, so the oldest take in a stack is still heard.
pub const GAIN_FLOOR: f32 = 0.05;

/// Each earlier take is quieter than its successor by this factor, applied once
/// per recorded bar.
pub const TAKE_DECAY: f32 = 0.7;

/// Most takes one recording session keeps, and the widest window a pattern can
/// average over.
///
/// The *default* window is a UI choice (`SINKO_SMOOTH_DEFAULT` in `tui`); this is
/// the bound, not the policy.
pub const MAX_TAKES: usize = 8;

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------

/// Why a pattern could not be built.
#[derive(Debug, PartialEq)]
pub enum RhythmError {
    /// The grid is not one of [`VALID_STEPS`], in steps per bar.
    BadResolution(usize),
    /// A step string held something other than `x` or `-`.
    BadStepChar(char),
    /// A pattern must have at least one layer.
    EmptyPattern,
    /// Every layer of a pattern shares one grid resolution.
    LayerLengthMismatch { expected: usize, found: usize },
    /// A hold must be between one tick and the whole bar.
    BadHold(u32),
    /// The muted tail must be no longer than a quarter note.
    BadMute(u32),
    /// A per-cell shape array was neither absent nor one entry per cell.
    ShapeLengthMismatch {
        what: &'static str,
        expected: usize,
        found: usize,
    },
    /// An accent must be a fraction of full level.
    BadVelocity(f32),
    /// Swing runs from 0 (straight) to 1 (the triplet feel).
    BadSwing(f32),
}

impl fmt::Display for RhythmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RhythmError::BadResolution(n) => write!(
                f,
                "{} steps per bar is not a grid resolution (use {})",
                n,
                VALID_STEPS
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            RhythmError::BadStepChar(c) => write!(
                f,
                "step string holds {:?}; use 'x' for a hit and '-' for a rest",
                c
            ),
            RhythmError::EmptyPattern => write!(f, "a pattern needs at least one layer"),
            RhythmError::LayerLengthMismatch { expected, found } => write!(
                f,
                "a layer has {} steps but the pattern's grid is {}",
                found, expected
            ),
            RhythmError::BadHold(v) => {
                write!(f, "a hold of {} ticks is outside 1..={}", v, BAR_TICKS)
            }
            RhythmError::BadMute(v) => {
                write!(
                    f,
                    "a muted tail of {} ticks is longer than {} (a quarter note)",
                    v, MAX_MUTE_TICKS
                )
            }
            RhythmError::ShapeLengthMismatch {
                what,
                expected,
                found,
            } => write!(
                f,
                "{} has {} entries but the grid has {} cells (or leave it out entirely)",
                what, found, expected
            ),
            RhythmError::BadVelocity(v) => {
                write!(f, "an accent of {} is outside 0..=1", v)
            }
            RhythmError::BadSwing(v) => {
                write!(f, "swing {} is outside 0..=1 (0 is straight)", v)
            }
        }
    }
}

impl std::error::Error for RhythmError {}

// -----------------------------------------------------------------------------
// Steps
// -----------------------------------------------------------------------------

/// True if `steps` is a grid this build understands.
pub fn valid_resolution(steps: usize) -> bool {
    VALID_STEPS.contains(&steps)
}

/// The grids whose cell *is* a triplet, so there is no straight offbeat pair left
/// for a swing amount to stretch.
pub const TRIPLET_STEPS: [usize; 2] = [12, 24];

/// True if a grid is already a triplet subdivision.
pub fn is_triplet_grid(steps: usize) -> bool {
    TRIPLET_STEPS.contains(&steps)
}

/// Parse the wire form of a step grid: `x` (or `X`) is a hit, `-` (or `.`) is a
/// rest.
///
/// A string rather than a list of booleans because a pattern is meant to be
/// readable and hand-editable in `rhythms.toml`, and because the length *is* the
/// grid resolution — there is no second field to disagree with it.
pub fn parse_steps(text: &str) -> Result<Vec<bool>, RhythmError> {
    let mut out = Vec::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'x' | 'X' => out.push(true),
            '-' | '.' => out.push(false),
            other => return Err(RhythmError::BadStepChar(other)),
        }
    }
    if !valid_resolution(out.len()) {
        return Err(RhythmError::BadResolution(out.len()));
    }
    Ok(out)
}

/// The wire form of a step grid.
pub fn render_steps(steps: &[bool]) -> String {
    steps
        .iter()
        .map(|on| if *on { 'x' } else { '-' })
        .collect()
}

// -----------------------------------------------------------------------------
// Layer
// -----------------------------------------------------------------------------

/// One take: a step grid and the gain it plays at.
#[derive(Clone, Debug, PartialEq)]
pub struct RhythmLayer {
    /// 0..1. Take 1 is loudest; each earlier take is decayed.
    pub gain: f32,
    /// One flag per grid cell.
    pub steps: Vec<bool>,
}

impl RhythmLayer {
    pub fn new(gain: f32, steps: Vec<bool>) -> Self {
        RhythmLayer {
            gain: gain.clamp(0.0, 1.0),
            steps,
        }
    }

    pub fn from_step_string(gain: f32, text: &str) -> Result<Self, RhythmError> {
        Ok(RhythmLayer::new(gain, parse_steps(text)?))
    }

    pub fn to_step_string(&self) -> String {
        render_steps(&self.steps)
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether this layer sounds on no step at all.
    pub fn is_empty(&self) -> bool {
        !self.any_hit()
    }

    pub fn hit(&self, step: usize) -> bool {
        self.steps.get(step).copied().unwrap_or(false)
    }

    pub fn any_hit(&self) -> bool {
        self.steps.iter().any(|on| *on)
    }

    /// The steps this layer sounds on, in order.
    pub fn hits(&self) -> Vec<usize> {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| i)
            .collect()
    }
}

// -----------------------------------------------------------------------------
// Pattern
// -----------------------------------------------------------------------------

/// A one-bar rhythm: a grid, a hold fraction, and the takes stacked on it.
///
/// `hold` is in **ticks**, so a half note stays a half note when the grid
/// changes. (It used to be a fraction of one cell, which meant switching from a
/// quarter grid to a sixteenth silently shortened every hold by four.) A slot
/// with no pattern assigned does not use a pattern at all — it sustains for the
/// transport's note length.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PatternWire", into = "PatternWire")]
pub struct RhythmPattern {
    pub name: String,
    /// The *default* hold for a hit, in ticks, `1..=BAR_TICKS`.
    ///
    /// A cell with its own entry in [`RhythmPattern::holds`] overrides it, which
    /// is how a Charleston gets its dotted quarter without every other hit in the
    /// pattern growing too.
    pub hold: u32,
    /// Per-cell hold overrides, one per grid cell. `0` means "use [`Self::hold`]".
    ///
    /// A `Vec` rather than a map because it is indexed by the same cell number as
    /// the step grid, so editing the grid cannot shift an override onto a
    /// neighbouring hit. It serialises only when some cell is set.
    pub holds: Vec<u32>,
    /// Per-cell accent, one per grid cell, `0.0..=1.0`. `1.0` is full level.
    ///
    /// Multiplied by the layer's own gain, so a take can still sit under another
    /// while a metre accent sits on top of both.
    pub velocities: Vec<f32>,
    /// How much of the bar's tail is silent, in ticks, up to a quarter note.
    ///
    /// 0 means no mute at all — the default, and what every pattern written
    /// before this field existed reads as.
    pub mute_ticks: u32,
    /// This pattern's swing, or `None` to follow the transport's.
    ///
    /// `0.0` is straight and `1.0` is the triplet feel — the same scale the
    /// transport's own swing uses, so an override is comparable with the default
    /// it replaces. A pattern that is *written* on the triplet grid ignores it:
    /// there is nothing left to swing.
    pub swing: Option<f32>,
    pub layers: Vec<RhythmLayer>,
}

impl RhythmPattern {
    /// A valid but silent draft at `resolution` steps per bar.
    pub fn blank(name: impl Into<String>, resolution: usize) -> Result<Self, RhythmError> {
        if !valid_resolution(resolution) {
            return Err(RhythmError::BadResolution(resolution));
        }
        Ok(RhythmPattern {
            name: name.into(),
            hold: DEFAULT_HOLD,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![RhythmLayer::new(1.0, vec![false; resolution])],
        })
    }

    /// A silent draft at the default grid, for a caller that cannot fail.
    ///
    /// This is what the Sinko panel starts from before anything has been
    /// tapped, which is why it needs no `Result`.
    pub fn draft() -> Self {
        RhythmPattern {
            name: String::new(),
            hold: DEFAULT_HOLD,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![RhythmLayer::new(1.0, vec![false; DEFAULT_STEPS])],
        }
    }

    /// One layer, taken from a step string.
    ///
    /// `gate` is how many cells the hit holds, as a fraction — the pattern stores
    /// the resulting ticks, so the grid is baked in here and never again.
    pub fn from_step_string(
        name: impl Into<String>,
        gate: f32,
        text: &str,
    ) -> Result<Self, RhythmError> {
        let steps = parse_steps(text)?;
        let hold = hold_from_gate(gate, steps.len());
        Ok(RhythmPattern {
            name: name.into(),
            hold,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![RhythmLayer::new(1.0, steps)],
        })
    }

    /// The hold for one cell: its override, or the pattern's default.
    pub fn cell_hold(&self, step: usize) -> u32 {
        match self.holds.get(step).copied() {
            Some(over) if over > 0 => over,
            _ => self.hold,
        }
    }

    /// The accent for one cell, `0.0..=1.0`. A cell with no entry is full level,
    /// so a file written before accents existed reads as it always sounded.
    pub fn cell_velocity(&self, step: usize) -> f32 {
        self.velocities
            .get(step)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.0, 1.0)
    }

    /// Set one cell's hold override. Zero clears it back to the default.
    ///
    /// Resizes to the grid on first use, so the array is always either empty or
    /// exactly one entry per cell — which is what [`Self::validate`] insists on.
    pub fn set_cell_hold(&mut self, step: usize, ticks: u32) {
        let cells = self.steps_per_bar();
        if step >= cells {
            return;
        }
        if self.holds.len() != cells {
            self.holds = vec![0; cells];
        }
        self.holds[step] = if ticks == self.hold { 0 } else { ticks };
        if self.holds.iter().all(|h| *h == 0) {
            self.holds.clear();
        }
    }

    /// Set one cell's accent. `1.0` clears it back to full level.
    pub fn set_cell_velocity(&mut self, step: usize, velocity: f32) {
        let cells = self.steps_per_bar();
        if step >= cells {
            return;
        }
        if self.velocities.len() != cells {
            self.velocities = vec![1.0; cells];
        }
        self.velocities[step] = velocity.clamp(0.0, 1.0);
        if self
            .velocities
            .iter()
            .all(|v| (*v - 1.0).abs() < f32::EPSILON)
        {
            self.velocities.clear();
        }
    }

    /// Whether this cell's hold is an override rather than the pattern default.
    pub fn has_hold_override(&self, step: usize) -> bool {
        self.holds.get(step).copied().is_some_and(|h| h > 0)
    }

    /// Whether this cell carries an accent other than full level.
    pub fn has_velocity_override(&self, step: usize) -> bool {
        self.velocities
            .get(step)
            .is_some_and(|v| (*v - 1.0).abs() >= f32::EPSILON)
    }

    /// The swing to play at: this pattern's own, or the transport's default.
    pub fn swing_or(&self, transport_swing: f32) -> f32 {
        self.swing.unwrap_or(transport_swing).clamp(0.0, 1.0)
    }

    /// Grid cells in one bar. Zero only for a pattern with no layers, which
    /// [`RhythmPattern::validate`] refuses.
    pub fn steps_per_bar(&self) -> usize {
        self.layers.first().map(|l| l.len()).unwrap_or(0)
    }

    /// Ticks per grid cell.
    pub fn step_ticks(&self) -> u64 {
        let steps = self.steps_per_bar();
        if steps == 0 {
            BAR_TICKS
        } else {
            BAR_TICKS / steps as u64
        }
    }

    /// How long one hit holds, in ticks.
    pub fn hold_ticks(&self) -> u64 {
        (self.hold as u64).clamp(1, BAR_TICKS)
    }

    /// The tick from which the bar is silent, or `None` when nothing is muted.
    ///
    /// `None` rather than `Some(BAR_TICKS)` on purpose: a boundary that happens
    /// to sit on the bar line would still trim a hold that crosses it, and a
    /// hold crossing the bar line is exactly what an offset chord is for.
    pub fn mute_boundary(&self) -> Option<u64> {
        if self.mute_ticks == 0 {
            return None;
        }
        Some(BAR_TICKS.saturating_sub(self.mute_ticks.min(BAR_TICKS as u32) as u64))
    }


    pub fn any_hit(&self) -> bool {
        self.layers.iter().any(|l| l.any_hit())
    }

    /// Every hit as `(step, gain)`, in step order and then take order — the
    /// order the stack sounds in when several takes land on one cell.
    pub fn hits(&self) -> Vec<(usize, f32)> {
        let steps = self.steps_per_bar();
        let mut out = Vec::new();
        for step in 0..steps {
            for layer in &self.layers {
                if layer.hit(step) {
                    out.push((step, layer.gain));
                }
            }
        }
        out
    }

    /// Total hits across every layer.
    pub fn hit_count(&self) -> usize {
        self.layers.iter().map(|l| l.hits().len()).sum()
    }

    pub fn validate(&self) -> Result<(), RhythmError> {
        let Some(first) = self.layers.first() else {
            return Err(RhythmError::EmptyPattern);
        };
        if !valid_resolution(first.len()) {
            return Err(RhythmError::BadResolution(first.len()));
        }
        for layer in &self.layers {
            if layer.len() != first.len() {
                return Err(RhythmError::LayerLengthMismatch {
                    expected: first.len(),
                    found: layer.len(),
                });
            }
        }
        if self.hold == 0 || self.hold as u64 > BAR_TICKS {
            return Err(RhythmError::BadHold(self.hold));
        }
        if self.mute_ticks > MAX_MUTE_TICKS {
            return Err(RhythmError::BadMute(self.mute_ticks));
        }
        // An override array is either absent or exactly one entry per cell: a
        // short one would silently apply the wrong cell's hold to the tail of the
        // bar, which is worse than refusing the file.
        let cells = first.len();
        for over in &self.holds {
            if *over as u64 > BAR_TICKS {
                return Err(RhythmError::BadHold(*over));
            }
        }
        if !self.holds.is_empty() && self.holds.len() != cells {
            return Err(RhythmError::ShapeLengthMismatch {
                what: "holds",
                expected: cells,
                found: self.holds.len(),
            });
        }
        if !self.velocities.is_empty() && self.velocities.len() != cells {
            return Err(RhythmError::ShapeLengthMismatch {
                what: "velocities",
                expected: cells,
                found: self.velocities.len(),
            });
        }
        for velocity in &self.velocities {
            if !(0.0..=1.0).contains(velocity) {
                return Err(RhythmError::BadVelocity(*velocity));
            }
        }
        if let Some(swing) = self.swing {
            if !(0.0..=1.0).contains(&swing) {
                return Err(RhythmError::BadSwing(swing));
            }
        }
        Ok(())
    }
}

/// Quiet every take one step of the decay chain.
///
/// Applied once per recorded bar, so the take you just played is always the
/// loudest and the stack fades as it builds.
pub fn decay_layer_gains(layers: &mut [RhythmLayer], factor: f32) {
    for layer in layers {
        layer.gain = (layer.gain * factor).clamp(GAIN_FLOOR, 1.0);
    }
}

// -----------------------------------------------------------------------------
// Tapping maths
// -----------------------------------------------------------------------------

/// Snap tapped tick positions onto a grid of `steps` cells per bar.
///
/// Each tap rounds to the nearest cell. Positions are taken modulo the bar, so a
/// take that ran across several bars folds into one — which is what makes a
/// one-bar pattern out of a four-bar performance.
pub fn quantize(taps: &[u32], steps: usize) -> Vec<bool> {
    if !valid_resolution(steps) {
        return Vec::new();
    }
    let step_ticks = BAR_TICKS as f64 / steps as f64;
    let mut grid = vec![false; steps];
    for &tap in taps {
        let position = (tap as f64).rem_euclid(BAR_TICKS as f64);
        let index = ((position / step_ticks).round() as i64).rem_euclid(steps as i64);
        grid[index as usize] = true;
    }
    grid
}

/// Average the n-th tap of the most recent `keep` takes.
///
/// Averaging by ordinal is what makes this *rough* rather than exact: the same
/// gesture played three times lands in three slightly different places, and the
/// mean is the gesture. Takes are truncated to the shortest one's tap count,
/// because a take with fewer taps has nothing to average against and inventing a
/// position would be worse than dropping the tail.
pub fn average_takes(takes: &[Vec<u32>], keep: usize) -> Vec<u32> {
    if takes.is_empty() || keep == 0 {
        return Vec::new();
    }
    let start = takes.len().saturating_sub(keep);
    let window = &takes[start..];
    let count = window.iter().map(|t| t.len()).min().unwrap_or(0);
    (0..count)
        .map(|i| {
            let sum: u64 = window.iter().map(|t| t[i] as u64).sum();
            (sum / window.len() as u64) as u32
        })
        .collect()
}

/// Where inside the bar `now` falls, in ticks.
///
/// The scheduler publishes each bar's start instant, and the UI timestamps key
/// presses against it; this is the whole clock behind tap capture. A bar that
/// has already ended wraps, so a late tick still lands somewhere sensible rather
/// than at the bar line.
pub fn bar_phase_ticks(now: Instant, bar_start: Instant, bar_duration: Duration) -> u32 {
    let total = bar_duration.as_secs_f64();
    if total <= 0.0 {
        return 0;
    }
    let elapsed = now.saturating_duration_since(bar_start).as_secs_f64();
    let fraction = (elapsed / total).rem_euclid(1.0);
    let ticks = (fraction * BAR_TICKS as f64).floor() as u64;
    ticks.min(BAR_TICKS - 1) as u32
}

// -----------------------------------------------------------------------------
// Built-in patterns
// -----------------------------------------------------------------------------

/// Build one single-layer built-in, or panic if the grid is malformed.
///
/// A built-in that does not validate is a programmer error, not a user one, so
/// this is deliberately not a `Result`: `rhythms.toml` loading is where a bad
/// grid has to be reported gently.
fn builtin(name: &str, gate: f32, steps: &str) -> RhythmPattern {
    RhythmPattern::from_step_string(name, gate, steps)
        .unwrap_or_else(|e| panic!("built-in pattern {} is invalid: {}", name, e))
}

/// One pattern built from repeated `x`s, so a 32-step roll is not a wall of
/// counted characters in the source.
fn builtin_pulse(name: &str, gate: f32, steps: usize) -> RhythmPattern {
    builtin(name, gate, &"x".repeat(steps))
}

/// The patterns every install ships with, written when `rhythms.toml` is
/// missing and merged into a file that predates them.
///
/// Ordered by feel rather than by age — sustained, then straight, then
/// sixteenths, then triplets, then the phrase sets — because the `pattern` row
/// cycles this list one press at a time and neighbours should sound related.
pub fn builtin_patterns() -> Vec<RhythmPattern> {
    // Every quarter, damped almost at once and with the last quarter muted: the
    // tight "chk" of a rhythm-guitar bar, and the one built-in that ships with a
    // mute so the row is discoverable from the palette.
    let mut damped = builtin("Damped Quarters", 0.4, "xxxx");
    damped.mute_ticks = 960;

    // The downbeats at full level, the offbeats under them. This used to be a
    // second, quieter layer, which was the only way an older build could say
    // "quieter"; per-cell velocity says it directly.
    let mut accented = builtin("Accented Eighths", 1.0, "x-x-x-x-");
    for step in [2, 4, 6] {
        accented.set_cell_velocity(step, 0.55);
    }

    // Charleston: the dotted quarter is a *length*, not just a long gap — the
    // first hit rings for a dotted quarter and the answer is an eighth.
    let mut charleston = builtin("Charleston", 2.0, "x-----x---------");
    charleston.set_cell_hold(0, 1440);

    // Tresillo's 3+3+2 is three lengths, so the pattern carries all three: two
    // dotted quarters and a quarter to close the bar.
    let mut tresillo = builtin("Tresillo", 4.0, "x-----x-----x---");
    tresillo.set_cell_hold(0, 1440);
    tresillo.set_cell_hold(6, 1440);

    let mut patterns = vec![
        // ---- sustained: one hit, held for a note value ----
        // The longest hold there is: a pad or a pedal.
        builtin("Held Whole", 4.0, "x---"),
        builtin("Held 3/4", 3.0, "x---"),
        builtin("Held Half", 2.0, "x---"),
        // The jazz "two feel": half notes on 1 and 3, room to comp over.
        builtin("Two Feel", 2.0, "x-x-"),
        // ---- straight ----
        // Four to the bar: the name is the note value, like every pattern here.
        builtin("Quarters", 0.8, "xxxx"),
        // ---- eighth grid ----
        builtin("Eighths", 0.5, "xxxxxxxx"),
        builtin("Offbeat Eighths", 0.5, "-x-x-x-x"),
        // ---- sixteenth grid ----
        // The sixteenth offbeats: the "e" and "a" of every beat, which is the
        // gap between `Offbeat Eighths` (the "&") and `Sixteenth Pulse`.
        builtin("Offbeat 16ths", 0.4, "-x-x-x-x-x-x-x-x"),
        // Dembow: the reggaeton cell — a 3-3-2 figure with the beat displaced,
        // so the same bar answers itself twice.
        builtin("Dembow", 2.0, "x--x--x-x--x--x-"),
        // Charleston: a dotted quarter on 1 answered by an eighth on the "&" of
        // 2 — the oldest two-note figure in jazz, and a comping staple.
        charleston,
        // Tresillo: 3+3+2, the cell underneath most Latin and much pop.
        tresillo,
        builtin("Syncopated 16ths", 0.5, "x--x--x-x--x-x--"),
        builtin_pulse("Sixteenth Pulse", 0.3, 16),
        // ---- triplet grid ----
        // Swung eighths: the first and third triplet of every beat, which is a
        // shuffle written straight. Long-short rather than even, on purpose.
        builtin("Swung Eighths", 1.0, "x-xx-xx-xx-x"),
        // ---- texture ----
        damped,
        accented,
        // A thirty-second roll: a fill for the last bar of a phrase.
        builtin_pulse("32nd Roll", 1.0, 32),
    ];

    // ---- phrases ----
    //
    // A pattern is one bar, so a longer figure is a *set* of one-bar patterns
    // that name their place in it: assign `Jazz Chorus 1/4`, `2/4`, `3/4` and
    // `4/4` to four consecutive chords and the four bars are the phrase. The
    // numbering is the whole contract, so the sets are kept together at the end
    // of the list, in order.
    patterns.extend(jazz_chorus());
    patterns.extend(son_clave());

    patterns
}

/// A four-bar jazz comp, one pattern per bar.
///
/// The arc is the one a player actually uses: state the pulse, answer it with a
/// Charleston, add the "&" of 3, then tighten the same figure into a turnaround
/// fill. Sixteenth grid throughout, short holds — comping, not pads.
fn jazz_chorus() -> Vec<RhythmPattern> {
    [
        // 1: beats 1 and 3 — the two-feel statement.
        "x-------x-------",
        // 2: Charleston, the answer.
        "x-----x---------",
        // 3: the same, plus the "&" of 3.
        "x-----x---x-----",
        // 4: the turnaround — two sixteenth pushes before the bar line.
        "x-----x---x-x-x-",
    ]
    .into_iter()
    .enumerate()
    .map(|(bar, steps)| builtin(&format!("Jazz Chorus {}/4", bar + 1), 2.0, steps))
    .collect()
}

/// The 3-2 son clave over two bars: the three-side, then the two-side.
///
/// The two halves of one clave, so they only make sense assigned to consecutive
/// chords; the eighth grid keeps the onsets exactly where the clave puts them.
fn son_clave() -> Vec<RhythmPattern> {
    [("x--x--x-", 1), ("--x-x---", 2)]
        .into_iter()
        .map(|(steps, side)| builtin(&format!("Son Clave {}/2", side), 1.0, steps))
        .collect()
}

// -----------------------------------------------------------------------------
// Wire form
// -----------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PatternWire {
    name: String,
    /// The hold in ticks — an integer, which is also what makes the file
    /// readable. Version 1 of this format wrote `gate`, a fraction of a grid
    /// cell; `TryFrom` converts one when it is the only thing present, so an old
    /// file keeps the sound it had.
    #[serde(default = "default_hold")]
    hold: u32,
    /// The old spelling, read only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gate: Option<f64>,
    /// Absent in every file written before this field existed, and skipped when
    /// zero, so a pattern with no mute does not carry a line saying so.
    #[serde(default, skip_serializing_if = "is_zero_mute")]
    mute: u32,
    /// Per-cell hold overrides, one entry per grid cell. Skipped when no cell has
    /// one, which is the case for every pattern that does not need it.
    #[serde(default, skip_serializing_if = "all_zero")]
    holds: Vec<u32>,
    /// Per-cell accents, one entry per grid cell. Skipped at full level.
    #[serde(default, skip_serializing_if = "all_full")]
    velocities: Vec<f32>,
    /// This pattern's swing, or absent to follow the transport's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    swing: Option<f32>,
    #[serde(default)]
    layers: Vec<LayerWire>,
}

fn is_zero_mute(value: &u32) -> bool {
    *value == 0
}

fn all_zero(values: &[u32]) -> bool {
    values.iter().all(|v| *v == 0)
}

fn all_full(values: &[f32]) -> bool {
    values.iter().all(|v| (*v - 1.0).abs() < f32::EPSILON)
}

fn default_hold() -> u32 {
    DEFAULT_HOLD
}

#[derive(Serialize, Deserialize)]
struct LayerWire {
    #[serde(default = "default_gain")]
    gain: f32,
    steps: String,
}



fn default_gain() -> f32 {
    1.0
}

impl From<RhythmPattern> for PatternWire {
    fn from(p: RhythmPattern) -> Self {
        PatternWire {
            name: p.name,
            hold: p.hold,
            gate: None,
            mute: p.mute_ticks,
            holds: p.holds,
            velocities: p.velocities,
            swing: p.swing,
            layers: p
                .layers
                .into_iter()
                .map(|layer| {
                    let steps = layer.to_step_string();
                    LayerWire {
                        gain: layer.gain,
                        steps,
                    }
                })
                .collect(),
        }
    }
}

impl TryFrom<PatternWire> for RhythmPattern {
    type Error = RhythmError;

    fn try_from(wire: PatternWire) -> Result<Self, RhythmError> {
        let mut layers = Vec::with_capacity(wire.layers.len());
        for layer in wire.layers {
            layers.push(RhythmLayer::from_step_string(layer.gain, &layer.steps)?);
        }
        // A file from before holds were measured in ticks: convert its gate
        // against the grid its own layers describe, which reproduces the sound
        // it had. `hold` is ignored in that case, because a v1 file has none.
        let steps = layers.first().map(|l| l.len()).unwrap_or(0);
        let hold = match wire.gate {
            Some(gate) => hold_from_gate(gate as f32, steps),
            None => wire.hold,
        };

        let pattern = RhythmPattern {
            name: wire.name,
            hold,
            holds: wire.holds,
            velocities: wire.velocities,
            mute_ticks: wire.mute,
            swing: wire.swing,
            layers,
        };
        pattern.validate()?;
        Ok(pattern)
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn taps(pairs: &[u32]) -> Vec<u32> {
        pairs.to_vec()
    }

    // ---- resolutions ----

    #[test]
    fn every_valid_resolution_divides_the_bar_exactly() {
        for steps in VALID_STEPS {
            assert!(valid_resolution(steps));
            assert_eq!(
                BAR_TICKS % steps as u64,
                0,
                "{} steps per bar leaves a partial step",
                steps
            );
        }
    }

    #[test]
    fn resolutions_between_the_allowed_ones_are_refused() {
        for steps in [0, 1, 3, 5, 6, 7, 48, 96, 128] {
            assert!(!valid_resolution(steps), "{} must not be a resolution", steps);
        }
    }

    #[test]
    fn the_triplet_grids_are_resolutions() {
        // The shuffle and 12/8 feels live here; without them every built-in was
        // straight, which is the hole this rung fills.
        for steps in [12, 24] {
            assert!(valid_resolution(steps));
            assert_eq!(BAR_TICKS % steps as u64, 0, "{} leaves a partial step", steps);
        }
        assert_eq!(BAR_TICKS / 12, 320, "an eighth-note triplet");
        assert_eq!(BAR_TICKS / 24, 160, "a sixteenth-note triplet");
    }

    #[test]
    fn a_half_note_bar_is_two_cells() {
        let p = RhythmPattern::from_step_string("h", 1.0, "x-").unwrap();
        assert_eq!(p.steps_per_bar(), 2);
        assert_eq!(p.step_ticks(), 1920);
        assert_eq!(p.hold_ticks(), 1920, "one cell is a half note");
    }

    #[test]
    fn step_ticks_follow_the_resolution() {
        let quarter = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        assert_eq!(quarter.steps_per_bar(), 4);
        assert_eq!(quarter.step_ticks(), 960);

        let sixteenth = RhythmPattern::from_step_string("s", 0.5, "xxxxxxxxxxxxxxxx").unwrap();
        assert_eq!(sixteenth.steps_per_bar(), 16);
        assert_eq!(sixteenth.step_ticks(), 240);

        let sixty_fourth = RhythmPattern::blank("b", 64).unwrap();
        assert_eq!(sixty_fourth.step_ticks(), 60);
    }

    // ---- step strings ----

    #[test]
    fn step_strings_round_trip() {
        let text = "x--x--x-x--x-x--";
        let pattern = RhythmPattern::from_step_string("sync", 0.5, text).unwrap();
        assert_eq!(pattern.layers[0].to_step_string(), text);
        assert_eq!(pattern.layers[0].hits(), vec![0, 3, 6, 8, 11, 13]);
    }

    #[test]
    fn dots_are_rests_and_caps_are_hits() {
        assert_eq!(parse_steps("x.X.").unwrap(), vec![true, false, true, false]);
    }

    #[test]
    fn a_bad_step_character_is_refused() {
        assert_eq!(parse_steps("xxo-"), Err(RhythmError::BadStepChar('o')));
    }

    #[test]
    fn a_step_string_of_the_wrong_length_is_refused() {
        assert_eq!(parse_steps("x"), Err(RhythmError::BadResolution(1)));
        // Five is not a resolution even though it is close to four.
        assert_eq!(parse_steps("x----"), Err(RhythmError::BadResolution(5)));
    }

    // ---- layers and validation ----

    #[test]
    fn a_blank_pattern_is_silent_but_valid() {
        let p = RhythmPattern::blank("new", DEFAULT_STEPS).unwrap();
        p.validate().unwrap();
        assert_eq!(p.steps_per_bar(), DEFAULT_STEPS);
        assert!(!p.any_hit());
        assert_eq!(p.hit_count(), 0);
        assert_eq!(p.hits(), Vec::new());
    }

    #[test]
    fn a_pattern_without_layers_is_refused() {
        let p = RhythmPattern {
            name: "empty".into(),
            hold: DEFAULT_HOLD,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: Vec::new(),
        };
        assert_eq!(p.validate(), Err(RhythmError::EmptyPattern));
    }

    #[test]
    fn layers_must_share_one_grid() {
        let p = RhythmPattern {
            name: "mixed".into(),
            hold: DEFAULT_HOLD,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![
                RhythmLayer::from_step_string(1.0, "x---").unwrap(),
                RhythmLayer::from_step_string(0.5, "xxxxxxxx").unwrap(),
            ],
        };
        assert_eq!(
            p.validate(),
            Err(RhythmError::LayerLengthMismatch {
                expected: 4,
                found: 8
            })
        );
    }

    #[test]
    fn a_hold_outside_the_range_is_refused() {
        let mut p = RhythmPattern::from_step_string("g", 0.5, "x---").unwrap();
        p.hold = BAR_TICKS as u32;
        p.validate().unwrap();
        p.hold = BAR_TICKS as u32 + 1;
        assert_eq!(
            p.validate(),
            Err(RhythmError::BadHold(BAR_TICKS as u32 + 1)),
            "a hold cannot outlive its bar"
        );
        p.hold = 0;
        assert_eq!(p.validate(), Err(RhythmError::BadHold(0)));
    }

    #[test]
    fn a_hold_in_ticks_does_not_move_when_the_grid_does() {
        // The point of measuring ticks: the same pattern on a finer grid still
        // holds a half note.
        let mut p = RhythmPattern::from_step_string("h", 2.0, "x---").unwrap();
        assert_eq!(p.hold, 1920);
        p.layers = vec![RhythmLayer::new(1.0, vec![false; 16])];
        assert_eq!(p.hold, 1920, "still a half note on a sixteenth grid");
        assert_eq!(p.hold_ticks(), 1920);
    }

    #[test]
    fn the_note_ladder_starts_where_the_player_asked_and_reaches_a_whole_note() {
        assert_eq!(
            &NOTE_LADDER[..5],
            &[120, 240, 480, 720, 960],
            "a 32nd, a 16th, an 8th, a 3/16, a quarter"
        );
        assert_eq!(*NOTE_LADDER.last().unwrap(), BAR_TICKS as u32);
        assert_eq!(MUTE_LADDER, [0, 120, 240, 480, 720, 960]);
        assert_eq!(MAX_MUTE_TICKS, 960, "a mute stops at a quarter note");
    }

    #[test]
    fn the_signed_ladder_is_symmetric_about_no_offset() {
        let rungs = signed_ladder();
        assert_eq!(rungs.first(), Some(&-3840));
        assert_eq!(rungs.last(), Some(&3840));
        assert_eq!(rungs[rungs.len() / 2], 0);
        assert_eq!(rungs.len(), NOTE_LADDER.len() * 2 + 1);
        for rung in &rungs {
            assert!(rungs.contains(&-rung), "{} has no mirror", rung);
        }
    }

    #[test]
    fn a_rung_step_moves_one_note_length() {
        let rungs: Vec<i32> = NOTE_LADDER.iter().map(|t| *t as i32).collect();
        assert_eq!(rung_step(&rungs, 240, 1), 480, "a 16th to an 8th");
        assert_eq!(rung_step(&rungs, 480, -1), 240);
        assert_eq!(rung_step(&rungs, 120, -1), 120, "clamped at the bottom");
        assert_eq!(rung_step(&rungs, 3840, 1), 3840, "and at the top");
    }

    #[test]
    fn a_rung_step_snaps_a_value_that_is_not_on_the_ladder() {
        // A hand-edited hold of 768 lands on the nearest rung, and only then
        // starts stepping.
        let rungs: Vec<i32> = NOTE_LADDER.iter().map(|t| *t as i32).collect();
        assert_eq!(rung_step(&rungs, 768, 1), 720, "snapped, not stepped past");
        assert_eq!(rung_step(&rungs, 768, -1), 720, "either way");
        assert_eq!(rung_step(&rungs, 1500, 1), 1440);
        assert_eq!(rung_step(&rungs, 3000, -1), 2880);
        assert_eq!(rung_step(&rungs, 4000, 1), 3840);
    }

    #[test]
    fn a_rung_step_over_the_signed_ladder_crosses_zero() {
        let rungs = signed_ladder();
        assert_eq!(rung_step(&rungs, 0, 1), 120, "off the beat, late");
        assert_eq!(rung_step(&rungs, 0, -1), -120, "or early");
        assert_eq!(rung_step(&rungs, -120, 1), 0, "and back through zero");
        assert_eq!(rung_step(&rungs, -3840, -1), -3840, "clamped");
    }

    #[test]
    fn a_hold_may_span_several_cells() {
        // `x---` on a quarter grid with a gate of 2 holds a half note; the same
        // pattern with a gate of 3 holds a dotted half.
        let half = RhythmPattern::from_step_string("h", 2.0, "x---").unwrap();
        assert_eq!(half.hold_ticks(), 1920);

        let dotted = RhythmPattern::from_step_string("d", 3.0, "x---").unwrap();
        assert_eq!(dotted.hold_ticks(), 2880);

        let whole = RhythmPattern::from_step_string("w", 4.0, "x---").unwrap();
        assert_eq!(whole.hold_ticks(), BAR_TICKS);
    }

    #[test]
    fn the_constructor_takes_a_cell_fraction_and_bakes_the_grid_in() {
        // "Two cells" means two of *this* pattern's cells, converted once at
        // construction; the pattern then stores ticks and the grid is irrelevant.
        let coarse = RhythmPattern::from_step_string("c", 2.0, "x---").unwrap();
        let fine = RhythmPattern::from_step_string("f", 2.0, "xxxxxxxxxxxxxxxx").unwrap();
        assert_eq!(coarse.hold_ticks(), 1920, "two quarter cells");
        assert_eq!(fine.hold_ticks(), 480, "two sixteenth cells");
    }

    #[test]
    fn hold_ticks_can_never_outlive_the_bar() {
        let mut p = RhythmPattern::from_step_string("g", 1.0, "xxxxxxxx").unwrap();
        p.hold = 99_999;
        assert_eq!(p.hold_ticks(), BAR_TICKS);
    }

    #[test]
    fn a_constructor_clamps_the_gate_to_the_grid() {
        // `from_step_string` sees the grid, so it can clamp rather than refuse:
        // nine cells of a four-cell bar is the whole bar.
        let p = RhythmPattern::from_step_string("g", 9.0, "x---").unwrap();
        assert_eq!(p.hold, BAR_TICKS as u32);
        p.validate().unwrap();
    }

    // ---- the muted tail ----

    #[test]
    fn the_mute_boundary_is_the_end_of_the_bar_minus_the_mute() {
        let mut p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        p.mute_ticks = 240;
        assert_eq!(p.mute_boundary(), Some(BAR_TICKS - 240));
        p.mute_ticks = MAX_MUTE_TICKS;
        assert_eq!(p.mute_boundary(), Some(BAR_TICKS - 960));
    }

    #[test]
    fn zero_mute_has_no_boundary_at_all() {
        // `None` rather than a boundary on the bar line: a boundary there would
        // still trim a hold that crosses the bar, which is what offsets are for.
        let p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        assert_eq!(p.mute_ticks, 0);
        assert_eq!(p.mute_boundary(), None);
    }

    #[test]
    fn a_mute_longer_than_a_quarter_note_is_refused() {
        let mut p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        p.mute_ticks = MAX_MUTE_TICKS;
        p.validate().unwrap();
        p.mute_ticks = MAX_MUTE_TICKS + 240;
        assert_eq!(
            p.validate(),
            Err(RhythmError::BadMute(MAX_MUTE_TICKS + 240))
        );
    }

    #[test]
    fn a_new_pattern_has_no_mute() {
        assert_eq!(RhythmPattern::draft().mute_ticks, 0);
        assert_eq!(RhythmPattern::blank("b", 16).unwrap().mute_ticks, 0);
    }

    #[test]
    fn the_mute_survives_a_toml_round_trip() {
        let mut pattern = RhythmPattern::from_step_string("Cut", 0.5, "x---").unwrap();
        pattern.mute_ticks = 480;
        assert_eq!(round_trip(pattern.clone()), pattern);
    }

    #[test]
    fn a_pattern_without_a_mute_in_the_file_reads_as_no_mute() {
        // Every rhythms.toml written before this field existed.
        let text = "[[rhythms]]\nname = \"Old\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].mute_ticks, 0);
    }

    #[test]
    fn a_pattern_with_no_mute_does_not_write_the_field() {
        let pattern = RhythmPattern::from_step_string("Plain", 0.5, "x---").unwrap();
        let text = toml::to_string_pretty(&File {
            rhythms: vec![pattern],
        })
        .unwrap();
        assert!(!text.contains("mute"), "rendered:\n{}", text);
    }

    #[test]
    fn the_hold_is_written_as_whole_ticks() {
        // It used to be an f32 cell fraction, which `toml` promoted to f64 and
        // wrote as 0.800000011920929 — unreadable in a file meant to be edited.
        let pattern = RhythmPattern::from_step_string("Q", 0.8, "xxxx").unwrap();
        assert_eq!(pattern.hold, 768, "0.8 of a 960-tick cell");
        let text = toml::to_string_pretty(&File {
            rhythms: vec![pattern.clone()],
        })
        .unwrap();
        assert!(text.contains("hold = 768"), "rendered:\n{}", text);
        assert!(!text.contains("gate"), "the old spelling is read-only");
        assert_eq!(round_trip(pattern.clone()), pattern);
    }

    #[test]
    fn a_file_from_before_ticks_keeps_the_sound_it_had() {
        // Version 1 wrote `gate`, a fraction of a cell. Reading it must convert
        // against the grid the file's own layers describe.
        let text = "[[rhythms]]\nname = \"Old\"\ngate = 0.5\n\n\
                    [[rhythms.layers]]\nsteps = \"xxxxxxxx\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].hold, 240, "half of an eighth-note cell");

        // And the same gate on a coarser grid is a longer hold, as it was.
        let coarse = "[[rhythms]]\nname = \"Old\"\ngate = 0.5\n\n\
                      [[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(coarse).unwrap();
        assert_eq!(file.rhythms[0].hold, 480);
    }

    #[test]
    fn the_quarters_built_in_is_four_quarter_notes() {
        // It used to be `x---`: one chord per bar under a name that means four
        // quarter notes.
        let quarters = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "Quarters")
            .expect("Quarters");
        assert_eq!(quarters.steps_per_bar(), 4);
        assert_eq!(quarters.layers[0].hits(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn the_held_built_ins_hold_for_their_note_value() {
        let patterns = builtin_patterns();
        let held = |name: &str| {
            patterns
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{}", name))
                .clone()
        };

        let half = held("Held Half");
        assert_eq!(half.layers[0].hits(), vec![0], "one hit, on the downbeat");
        assert_eq!(half.hold_ticks(), 1920, "a half note");

        let dotted = held("Held 3/4");
        assert_eq!(dotted.layers[0].hits(), vec![0]);
        assert_eq!(dotted.hold_ticks(), 2880, "a dotted half");
    }

    #[test]
    fn hold_ticks_are_one_step_times_the_gate() {
        let p = RhythmPattern::from_step_string("q", 1.0, "x---").unwrap();
        assert_eq!(p.hold_ticks(), 960);
        let p = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        assert_eq!(p.hold_ticks(), 480);
        // Past one cell, which is how a held half note is written.
        let p = RhythmPattern::from_step_string("h", 2.0, "x---").unwrap();
        assert_eq!(p.hold_ticks(), 1920);
        // Always at least one tick, so a zero-length note never reaches the
        // writer.
        let mut tiny = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        tiny.hold = 0;
        assert_eq!(tiny.hold_ticks(), 1);
    }

    #[test]
    fn layer_gains_are_clamped_on_construction() {
        assert_eq!(RhythmLayer::new(4.0, vec![false; 4]).gain, 1.0);
        assert_eq!(RhythmLayer::new(-1.0, vec![false; 4]).gain, 0.0);
    }

    #[test]
    fn hits_are_ordered_by_step_then_take() {
        let p = RhythmPattern {
            name: "stack".into(),
            hold: DEFAULT_HOLD,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![
                RhythmLayer::from_step_string(1.0, "x---").unwrap(),
                RhythmLayer::from_step_string(0.7, "x---").unwrap(),
            ],
        };
        assert_eq!(p.hits(), vec![(0, 1.0), (0, 0.7)]);
        assert_eq!(p.hit_count(), 2);
    }

    // ---- decay ----

    #[test]
    fn decaying_quiets_every_layer_and_stops_at_the_floor() {
        let mut layers = vec![
            RhythmLayer::from_step_string(1.0, "x---").unwrap(),
            RhythmLayer::from_step_string(0.5, "x---").unwrap(),
        ];
        decay_layer_gains(&mut layers, TAKE_DECAY);
        assert!((layers[0].gain - 0.7).abs() < 1e-6);
        assert!((layers[1].gain - 0.35).abs() < 1e-6);

        for _ in 0..50 {
            decay_layer_gains(&mut layers, TAKE_DECAY);
        }
        assert_eq!(layers[0].gain, GAIN_FLOOR);
        assert_eq!(layers[1].gain, GAIN_FLOOR);
    }

    // ---- quantization ----

    #[test]
    fn quantization_snaps_to_the_nearest_cell() {
        // At 16 steps a cell is 240 ticks. 100 is nearest cell 0, 140 nearest 1.
        assert_eq!(quantize(&[100], 16), {
            let mut g = vec![false; 16];
            g[0] = true;
            g
        });
        assert_eq!(quantize(&[300], 16), {
            let mut g = vec![false; 16];
            g[1] = true;
            g
        });
    }

    #[test]
    fn quantization_rounds_the_halfway_point_up() {
        // 120 is exactly half a 240-tick cell.
        let grid = quantize(&[120], 16);
        assert!(grid[1], "half a cell should round up, got {:?}", grid);
    }

    #[test]
    fn quantization_folds_a_multi_bar_take_into_one_bar() {
        // The same gesture in bar 1 and bar 3 lands on one cell.
        let grid = quantize(&[10, BAR_TICKS as u32 + 10], 16);
        assert_eq!(grid.iter().filter(|on| **on).count(), 1);
        assert!(grid[0]);
    }

    #[test]
    fn quantization_wraps_a_tap_before_the_bar_line() {
        // Just under the bar line rounds forward to the next bar's downbeat.
        let grid = quantize(&[(BAR_TICKS - 10) as u32], 16);
        assert!(grid[0], "expected a wrap to step 0, got {:?}", grid);
    }

    #[test]
    fn quantization_handles_every_resolution() {
        for steps in VALID_STEPS {
            let grid = quantize(&[0], steps);
            assert_eq!(grid.len(), steps);
            assert!(grid[0], "the downbeat must quantize to step 0");
            assert_eq!(grid.iter().filter(|on| **on).count(), 1);
        }
    }

    #[test]
    fn quantization_of_an_invalid_resolution_is_empty() {
        assert!(quantize(&[0], 7).is_empty());
    }

    #[test]
    fn duplicate_taps_on_one_cell_collapse() {
        let grid = quantize(&[0, 20, 30], 16);
        assert_eq!(grid.iter().filter(|on| **on).count(), 1);
    }

    // ---- averaging ----

    #[test]
    fn averaging_takes_works_by_ordinal() {
        let takes = vec![taps(&[0, 1000]), taps(&[10, 1010]), taps(&[20, 1020])];
        assert_eq!(average_takes(&takes, 3), vec![10, 1010]);
    }

    #[test]
    fn averaging_uses_only_the_most_recent_takes() {
        let takes = vec![taps(&[0]), taps(&[100]), taps(&[200]), taps(&[300])];
        assert_eq!(average_takes(&takes, 2), vec![250]);
    }

    #[test]
    fn averaging_truncates_to_the_shortest_take() {
        let takes = vec![taps(&[0, 100, 200]), taps(&[10, 110])];
        assert_eq!(average_takes(&takes, 3), vec![5, 105]);
    }

    #[test]
    fn averaging_nothing_is_empty() {
        assert_eq!(average_takes(&[], 3), Vec::<u32>::new());
        assert_eq!(average_takes(&[taps(&[1, 2])], 0), Vec::<u32>::new());
    }

    #[test]
    fn averaging_more_takes_than_exist_uses_them_all() {
        let takes = vec![taps(&[0, 100]), taps(&[10, 110])];
        assert_eq!(average_takes(&takes, 99), vec![5, 105]);
    }

    // ---- bar phase ----

    #[test]
    fn bar_phase_walks_the_bar() {
        let start = Instant::now();
        let bar = Duration::from_secs(2);
        assert_eq!(bar_phase_ticks(start, start, bar), 0);
        assert_eq!(
            bar_phase_ticks(start + Duration::from_millis(500), start, bar),
            960
        );
        assert_eq!(
            bar_phase_ticks(start + Duration::from_millis(1000), start, bar),
            1920
        );
    }

    #[test]
    fn bar_phase_never_reaches_the_bar_line() {
        let start = Instant::now();
        let bar = Duration::from_secs(2);
        // Exactly one bar later is the next downbeat, not tick 3840.
        assert_eq!(bar_phase_ticks(start + bar, start, bar), 0);
        assert_eq!(
            bar_phase_ticks(start + bar - Duration::from_micros(1), start, bar),
            BAR_TICKS as u32 - 1
        );
    }

    #[test]
    fn bar_phase_before_the_bar_start_is_the_downbeat() {
        let start = Instant::now();
        let before = start - Duration::from_millis(50);
        assert_eq!(bar_phase_ticks(before, start, Duration::from_secs(2)), 0);
    }

    #[test]
    fn bar_phase_of_a_zero_length_bar_is_the_downbeat() {
        let now = Instant::now();
        assert_eq!(bar_phase_ticks(now, now, Duration::ZERO), 0);
    }

    // ---- wire form ----

    #[derive(Debug, Serialize, Deserialize)]
    struct File {
        rhythms: Vec<RhythmPattern>,
    }

    fn round_trip(pattern: RhythmPattern) -> RhythmPattern {
        let file = File {
            rhythms: vec![pattern],
        };
        let text = toml::to_string_pretty(&file).unwrap();
        let back: File = toml::from_str(&text).unwrap();
        assert_eq!(back.rhythms.len(), 1);
        back.rhythms.into_iter().next().unwrap()
    }

    #[test]
    fn a_pattern_round_trips_through_toml() {
        let pattern = RhythmPattern {
            name: "Stack".into(),
            hold: 480,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
            layers: vec![
                RhythmLayer::from_step_string(1.0, "x--x--x-").unwrap(),
                RhythmLayer::from_step_string(0.7, "--------").unwrap(),
            ],
        };
        assert_eq!(round_trip(pattern.clone()), pattern);
    }

    #[test]
    fn the_serialized_form_is_the_readable_step_string() {
        let pattern = RhythmPattern::from_step_string("Offbeat", 0.5, "-x-x-x-x").unwrap();
        let file = File {
            rhythms: vec![pattern],
        };
        let text = toml::to_string_pretty(&file).unwrap();
        assert!(text.contains(r#"steps = "-x-x-x-x""#), "rendered:\n{}", text);
        assert!(text.contains(r#"name = "Offbeat""#), "rendered:\n{}", text);
    }

    #[test]
    fn toml_rejects_a_bad_step_string() {
        let text = "[[rhythms]]\nname = \"x\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"xxo-\"\n";
        let err = toml::from_str::<File>(text).unwrap_err().to_string();
        assert!(err.contains('o'), "message was: {}", err);
    }

    #[test]
    fn toml_rejects_a_bad_resolution() {
        let text = "[[rhythms]]\nname = \"x\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"x---x\"\n";
        assert!(toml::from_str::<File>(text).is_err());
    }

    #[test]
    fn toml_rejects_layer_length_mismatch() {
        let text = "[[rhythms]]\nname = \"x\"\ngate = 0.5\n\n\
                    [[rhythms.layers]]\nsteps = \"x---\"\n\n\
                    [[rhythms.layers]]\nsteps = \"xxxxxxxx\"\n";
        let err = toml::from_str::<File>(text).unwrap_err().to_string();
        assert!(err.contains("grid"), "message was: {}", err);
    }

    #[test]
    fn toml_rejects_a_pattern_with_no_layers() {
        let text = "[[rhythms]]\nname = \"x\"\ngate = 0.5\n";
        assert!(toml::from_str::<File>(text).is_err());
    }

    #[test]
    fn a_layer_defaults_to_full_gain_and_a_pattern_to_the_default_hold() {
        let text = "[[rhythms]]\nname = \"Bare\"\n\n[[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].hold, DEFAULT_HOLD);
        assert_eq!(file.rhythms[0].layers[0].gain, 1.0);
    }

    // ---- built-ins ----

    #[test]
    fn every_built_in_pattern_is_valid() {
        for pattern in builtin_patterns() {
            pattern
                .validate()
                .unwrap_or_else(|e| panic!("{} is invalid: {}", pattern.name, e));
        }
    }

    #[test]
    fn built_ins_have_distinct_names_and_are_not_silent() {
        let patterns = builtin_patterns();
        assert_eq!(patterns.len(), 23, "the shipped palette");
        let mut names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), patterns.len(), "duplicate built-in name");
        for pattern in &patterns {
            assert!(pattern.any_hit(), "{} is silent", pattern.name);
        }
    }

    #[test]
    fn the_numbered_phrases_are_contiguous_and_in_order() {
        // A phrase is a set of one-bar patterns whose names carry the position,
        // so the numbering *is* the feature: a missing or reordered bar would
        // silently make the figure play wrong.
        let patterns = builtin_patterns();
        let names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();

        for (prefix, bars) in [("Jazz Chorus", 4usize), ("Son Clave", 2usize)] {
            let first = names
                .iter()
                .position(|n| n.starts_with(prefix))
                .unwrap_or_else(|| panic!("{} is missing", prefix));
            for bar in 1..=bars {
                let want = format!("{} {}/{}", prefix, bar, bars);
                assert_eq!(
                    names[first + bar - 1],
                    want,
                    "the phrase must run {} in order",
                    prefix
                );
            }
            // The set is closed: nothing of this phrase's family may sit outside
            // the run, or the palette would show a stray bar on its own.
            assert!(
                !names
                    .iter()
                    .skip(first + bars)
                    .any(|n| n.starts_with(prefix)),
                "{} must be one contiguous run",
                prefix
            );
        }
    }

    #[test]
    fn the_offbeat_built_in_is_actually_syncopated() {
        // The feature's whole point: hits that are not on the beat.
        let patterns = builtin_patterns();
        let offbeat = patterns
            .iter()
            .find(|p| p.name == "Offbeat Eighths")
            .expect("Offbeat Eighths");
        assert_eq!(offbeat.steps_per_bar(), 8);
        assert_eq!(offbeat.layers[0].hits(), vec![1, 3, 5, 7]);
        assert!(!offbeat.layers[0].hit(0), "an offbeat pattern has no downbeat");
    }

    #[test]
    fn the_added_built_ins_have_the_onsets_their_names_claim() {
        // The step strings are hand-written, so the grid they produce is the one
        // thing worth pinning: a dropped dash would silently turn a tresillo into
        // a straight bar and the name would still read right.
        let patterns = builtin_patterns();
        let onsets = |name: &str| -> (usize, Vec<usize>) {
            let pattern = patterns
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no built-in named {:?}", name));
            (pattern.steps_per_bar(), pattern.layers[0].hits())
        };

        // Two halves on 1 and 3.
        assert_eq!(onsets("Two Feel"), (4, vec![0, 2]));
        // Dotted quarter on 1, eighth on the "&" of 2.
        assert_eq!(onsets("Charleston"), (16, vec![0, 6]));
        // 3+3+2.
        assert_eq!(onsets("Tresillo"), (16, vec![0, 6, 12]));
        // The "e" and "a" of every beat, and no downbeat.
        assert_eq!(
            onsets("Offbeat 16ths"),
            (16, vec![1, 3, 5, 7, 9, 11, 13, 15])
        );
        // Dembow: the tresillo cell with the beat displaced.
        assert_eq!(onsets("Dembow"), (16, vec![0, 3, 6, 8, 11, 14]));
        // Swung eighths: first and third triplet of every beat.
        assert_eq!(onsets("Swung Eighths"), (12, vec![0, 2, 3, 5, 6, 8, 9, 11]));
        // A roll fills its grid.
        assert_eq!(onsets("32nd Roll"), (32, (0..32).collect::<Vec<_>>()));

        // The four-bar jazz comp: pulse, Charleston, add the "&" of 3, fill.
        assert_eq!(onsets("Jazz Chorus 1/4"), (16, vec![0, 8]));
        assert_eq!(onsets("Jazz Chorus 2/4"), (16, vec![0, 6]));
        assert_eq!(onsets("Jazz Chorus 3/4"), (16, vec![0, 6, 10]));
        assert_eq!(onsets("Jazz Chorus 4/4"), (16, vec![0, 6, 10, 12, 14]));
        // 3-2 son clave: the three-side, then the two-side.
        assert_eq!(onsets("Son Clave 1/2"), (8, vec![0, 3, 6]));
        assert_eq!(onsets("Son Clave 2/2"), (8, vec![2, 4]));
    }

    #[test]
    fn held_whole_is_the_longest_hold_there_is() {
        let pattern = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "Held Whole")
            .expect("Held Whole");
        assert_eq!(pattern.hold_ticks(), BAR_TICKS, "one hit, the whole bar");
        assert_eq!(pattern.layers[0].hits(), vec![0]);
    }

    #[test]
    fn the_texture_built_ins_ship_their_mute_and_their_accents() {
        // These two exist to make a feature visible from the palette: nothing
        // else ships muted, and nothing else ships accented.
        let patterns = builtin_patterns();
        let damped = patterns
            .iter()
            .find(|p| p.name == "Damped Quarters")
            .expect("Damped Quarters");
        assert_eq!(
            damped.mute_boundary(),
            Some(BAR_TICKS - MAX_MUTE_TICKS as u64)
        );
        assert_eq!(damped.hold_ticks(), 384, "damped, not ringing");

        let accented = patterns
            .iter()
            .find(|p| p.name == "Accented Eighths")
            .expect("Accented Eighths");
        assert_eq!(accented.layers.len(), 1, "an accent is not a second layer");
        assert_eq!(accented.cell_velocity(0), 1.0, "the downbeat is full");
        for step in [2, 4, 6] {
            assert_eq!(accented.cell_velocity(step), 0.55, "the offbeats sit under");
            assert!(accented.has_velocity_override(step));
        }
        assert!(
            !accented.has_velocity_override(0),
            "no override at full level"
        );
    }

    #[test]
    fn the_shape_built_ins_carry_real_note_lengths() {
        // Charleston and the tresillo are the reason per-cell lengths exist: the
        // onsets were always right, but every hit used to ring for one value.
        let patterns = builtin_patterns();
        let find = |name: &str| {
            patterns
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no built-in named {:?}", name))
                .clone()
        };

        let charleston = find("Charleston");
        assert_eq!(charleston.hold, 480, "the answer is an eighth");
        assert_eq!(
            charleston.cell_hold(0),
            1440,
            "the statement is a dotted quarter"
        );
        assert_eq!(
            charleston.cell_hold(6),
            480,
            "and the default holds for the rest"
        );

        let tresillo = find("Tresillo");
        assert_eq!(tresillo.cell_hold(0), 1440);
        assert_eq!(tresillo.cell_hold(6), 1440);
        assert_eq!(tresillo.cell_hold(12), 960, "3+3+2 closes with a quarter");
    }

    // ---- per-cell shape ----

    #[test]
    fn a_cell_override_round_trips_and_clears() {
        let mut pattern = RhythmPattern::from_step_string("Shape", 1.0, "x-x-").unwrap();
        assert!(!pattern.has_hold_override(0));
        assert_eq!(pattern.cell_hold(0), pattern.hold, "the default, until set");

        pattern.set_cell_hold(0, 1440);
        assert!(pattern.has_hold_override(0));
        assert_eq!(pattern.cell_hold(0), 1440);
        assert_eq!(pattern.holds.len(), 4, "one entry per cell once used");
        assert_eq!(
            pattern.cell_hold(2),
            pattern.hold,
            "only the one cell moved"
        );

        // Setting it back to the default *clears* the override, so the pattern
        // does not carry a row of numbers saying nothing.
        pattern.set_cell_hold(0, pattern.hold);
        assert!(!pattern.has_hold_override(0));
        assert!(pattern.holds.is_empty(), "and the array is dropped");
    }

    #[test]
    fn accents_and_lengths_survive_the_wire() {
        let mut pattern = RhythmPattern::from_step_string("Shaped", 1.0, "x-x-").unwrap();
        pattern.set_cell_hold(0, 1440);
        pattern.set_cell_velocity(2, 0.5);
        pattern.swing = Some(0.4);

        let text = toml::to_string_pretty(&RhythmFileOne(pattern.clone())).unwrap();
        let back: RhythmFileOne = toml::from_str(&text).unwrap();
        assert_eq!(back.0, pattern, "a shaped pattern must round-trip:\n{}", text);

        // And an unshaped one writes none of it, so the file stays readable.
        let plain = RhythmPattern::from_step_string("Plain", 1.0, "xxxx").unwrap();
        let text = toml::to_string_pretty(&RhythmFileOne(plain.clone())).unwrap();
        assert!(!text.contains("holds"), "{}", text);
        assert!(!text.contains("velocities"), "{}", text);
        assert!(!text.contains("swing"), "{}", text);
        let back: RhythmFileOne = toml::from_str(&text).unwrap();
        assert_eq!(back.0, plain);
    }

    /// A one-pattern wrapper, so the wire form can be exercised without a file.
    #[derive(Serialize, Deserialize)]
    struct RhythmFileOne(RhythmPattern);

    #[test]
    fn a_shape_array_of_the_wrong_length_is_refused() {
        let mut pattern = RhythmPattern::from_step_string("Bad", 1.0, "x-x-").unwrap();
        pattern.holds = vec![1440, 0];
        assert_eq!(
            pattern.validate(),
            Err(RhythmError::ShapeLengthMismatch {
                what: "holds",
                expected: 4,
                found: 2,
            })
        );
    }

    #[test]
    fn swing_reads_the_patterns_own_or_the_transports() {
        let mut pattern = RhythmPattern::from_step_string("S", 1.0, "x-x-").unwrap();
        assert_eq!(
            pattern.swing_or(0.6),
            0.6,
            "no override follows the transport"
        );
        pattern.swing = Some(0.2);
        assert_eq!(pattern.swing_or(0.6), 0.2, "an override wins");

        pattern.swing = Some(4.0);
        assert!(pattern.validate().is_err(), "and it is still range-checked");
    }
}

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
    /// A pattern must have at least one bar.
    EmptyPattern,
    /// A pattern may hold at most [`MAX_PATTERN_BARS`] bars.
    TooManyBars(usize),
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
            RhythmError::EmptyPattern => write!(f, "a pattern needs at least one bar"),
            RhythmError::TooManyBars(n) => write!(
                f,
                "a pattern holds {} bars; the most is {}",
                n, MAX_PATTERN_BARS
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
// Bar
// -----------------------------------------------------------------------------

/// One bar of hits: a grid and the shapes played on it.
///
/// This used to be a *layer* — one take of a tapped performance, stacked with the
/// takes before it and decayed a step each. The stack is gone: a bar is now the
/// unit a progression entry owns and the unit a pattern is made of, and the
/// overlapping-takes sound it existed for is what the four-bar looper records
/// instead, one bar at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct RhythmBar {
    /// One flag per grid cell.
    pub steps: Vec<bool>,
    /// Per-cell hold overrides, one entry per grid cell. `0` means "use `hold`".
    pub holds: Vec<u32>,
    /// Per-cell accent, one per grid cell, `0.0..=1.0`. `1.0` is full level.
    pub velocities: Vec<f32>,
    /// The *default* hold for a hit, in ticks, `1..=BAR_TICKS`.
    pub hold: u32,
    /// How much of the bar's tail is silent, in ticks, up to a quarter note.
    pub mute_ticks: u32,
    /// This bar's swing, or `None` to follow the transport's.
    pub swing: Option<f32>,
}

impl RhythmBar {
    /// A silent bar at `resolution` steps, holding like `hold`.
    pub fn silent(resolution: usize, hold: u32) -> Self {
        RhythmBar {
            steps: vec![false; resolution],
            holds: Vec::new(),
            velocities: Vec::new(),
            hold,
            mute_ticks: 0,
            swing: None,
        }
    }

    /// A bar from a step string, with `gate` as the hold in cells.
    ///
    /// `gate` is how many cells the hit holds, as a fraction — the bar stores the
    /// resulting ticks, so the grid is baked in here and never again.
    pub fn from_step_string(gate: f32, text: &str) -> Result<Self, RhythmError> {
        let steps = parse_steps(text)?;
        Ok(RhythmBar {
            hold: hold_from_gate(gate, steps.len()),
            steps,
            holds: Vec::new(),
            velocities: Vec::new(),
            mute_ticks: 0,
            swing: None,
        })
    }

    /// The wire form of the grid.
    pub fn to_step_string(&self) -> String {
        render_steps(&self.steps)
    }

    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        !self.any_hit()
    }

    pub fn hit(&self, step: usize) -> bool {
        self.steps.get(step).copied().unwrap_or(false)
    }

    pub fn any_hit(&self) -> bool {
        self.steps.iter().any(|on| *on)
    }

    /// Grid cells in one bar.
    pub fn steps_per_bar(&self) -> usize {
        self.steps.len()
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

    /// The hold for one cell: its override, or the bar's default.
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

    /// Whether this cell's hold is an override rather than the bar default.
    pub fn has_hold_override(&self, step: usize) -> bool {
        self.holds.get(step).copied().is_some_and(|h| h > 0)
    }

    /// Whether this cell carries an accent other than full level.
    pub fn has_velocity_override(&self, step: usize) -> bool {
        self.velocities
            .get(step)
            .is_some_and(|v| (*v - 1.0).abs() >= f32::EPSILON)
    }

    /// The swing to play at: this bar's own, or the transport's default.
    pub fn swing_or(&self, transport_swing: f32) -> f32 {
        self.swing.unwrap_or(transport_swing).clamp(0.0, 1.0)
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

    /// Every hit as `(step, 1.0)`, in step order.
    ///
    /// The second value was the take's gain; with one grid there is nothing for
    /// it to say, and it stays in the shape because every caller multiplies it
    /// into the stab's loudness.
    pub fn hits(&self) -> Vec<(usize, f32)> {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(step, _)| (step, 1.0))
            .collect()
    }

    /// The steps this bar sounds on, in order.
    pub fn hit_steps(&self) -> Vec<usize> {
        self.steps
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn hit_count(&self) -> usize {
        self.steps.iter().filter(|on| **on).count()
    }

    /// The cells a hit is *still sounding* through, one flag per grid cell.
    ///
    /// `false` everywhere a hit starts — an onset is drawn as a hit, not as a
    /// sustain — and true on the cells after it that its hold covers. It is what
    /// lets a grid show a note held for a bar as a line rather than a dot, which
    /// is the difference between a four-bar loop you can read and one you have
    /// to play back to hear.
    ///
    /// Clipped at the bar: a hold that carries into the next bar marks nothing
    /// there, because the next bar has its own grid and its own hits.
    pub fn sustain(&self) -> Vec<bool> {
        let mut out = vec![false; self.steps.len()];
        let step_ticks = self.step_ticks().max(1);
        for (step, on) in self.steps.iter().enumerate() {
            if !on {
                continue;
            }
            let cells = (self.cell_hold(step) as u64).div_ceil(step_ticks) as usize;
            for covered in (step + 1)..(step + cells).min(out.len()) {
                out[covered] = true;
            }
        }
        out
    }

    /// Clear every hit, keeping the grid and the shapes.
    pub fn clear_hits(&mut self) {
        for step in self.steps.iter_mut() {
            *step = false;
        }
    }

    pub fn validate(&self) -> Result<(), RhythmError> {
        if !valid_resolution(self.steps.len()) {
            return Err(RhythmError::BadResolution(self.steps.len()));
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
        let cells = self.steps.len();
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

// -----------------------------------------------------------------------------
// Pattern
// -----------------------------------------------------------------------------

/// Most bars one pattern may hold. Four, which is one looper pass and the deepest
/// stack the rhythm feature has ever had to sound.
pub const MAX_PATTERN_BARS: usize = 4;

/// A named rhythm: one to [`MAX_PATTERN_BARS`] bars.
///
/// A pattern is what the library holds and what a chord is assigned *from*; the
/// entry keeps one bar of it. One bar is the common case and every shipped
/// default, and four is what the tap looper records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PatternWire", into = "PatternWire")]
pub struct RhythmPattern {
    pub name: String,
    pub bars: Vec<RhythmBar>,
}

impl RhythmPattern {
    /// A pattern of one bar.
    pub fn single(name: impl Into<String>, bar: RhythmBar) -> Self {
        RhythmPattern {
            name: name.into(),
            bars: vec![bar],
        }
    }

    /// A valid but silent pattern of one bar at `resolution` steps per bar.
    pub fn blank(name: impl Into<String>, resolution: usize) -> Result<Self, RhythmError> {
        if !valid_resolution(resolution) {
            return Err(RhythmError::BadResolution(resolution));
        }
        Ok(RhythmPattern::single(
            name,
            RhythmBar::silent(resolution, DEFAULT_HOLD),
        ))
    }

    /// A silent one-bar draft at the default grid, for a caller that cannot fail.
    ///
    /// This is what the Sinko panel starts from before anything has been tapped,
    /// which is why it needs no `Result`.
    pub fn draft() -> Self {
        RhythmPattern::single(
            String::new(),
            RhythmBar::silent(DEFAULT_STEPS, DEFAULT_HOLD),
        )
    }

    /// A one-bar pattern from a step string, with `gate` as the hold in cells.
    pub fn from_step_string(
        name: impl Into<String>,
        gate: f32,
        text: &str,
    ) -> Result<Self, RhythmError> {
        Ok(RhythmPattern::single(
            name,
            RhythmBar::from_step_string(gate, text)?,
        ))
    }

    /// How many bars this pattern holds.
    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// The first bar, which is what a progression entry takes.
    pub fn first(&self) -> Option<&RhythmBar> {
        self.bars.first()
    }

    /// How many hits across every bar.
    pub fn hit_count(&self) -> usize {
        self.bars.iter().map(|bar| bar.hit_count()).sum()
    }

    /// Whether any bar sounds at all.
    pub fn any_hit(&self) -> bool {
        self.bars.iter().any(|bar| bar.any_hit())
    }

    pub fn validate(&self) -> Result<(), RhythmError> {
        if self.bars.is_empty() {
            return Err(RhythmError::EmptyPattern);
        }
        if self.bars.len() > MAX_PATTERN_BARS {
            return Err(RhythmError::TooManyBars(self.bars.len()));
        }
        for bar in &self.bars {
            bar.validate()?;
        }
        Ok(())
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

/// The same, as a whole bar holding at the default.
pub fn quantize_bar(taps: &[u32], steps: usize, hold: u32) -> RhythmBar {
    let mut bar = RhythmBar::silent(steps, hold);
    bar.steps = quantize(taps, steps);
    bar
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

/// The first bar of a pattern, for the single-bar shapes below.
///
/// A built-in is always at least one bar — `builtin` panics otherwise — so this
/// is the whole of "the pattern's bar" for a one-bar pattern.
fn first_bar(pattern: &mut RhythmPattern) -> &mut RhythmBar {
    pattern
        .bars
        .first_mut()
        .unwrap_or_else(|| panic!("built-in pattern {} has no bars", pattern.name))
}

/// A multi-bar pattern from one step string per bar.
///
/// Every bar shares the gate, which is what a phrase wants: the same touch
/// across the four bars, with the figure the only thing that changes.
fn builtin_phrase(name: &str, gate: f32, bars: &[&str]) -> RhythmPattern {
    let built = bars
        .iter()
        .map(|steps| {
            RhythmBar::from_step_string(gate, steps)
                .unwrap_or_else(|e| panic!("built-in pattern {} is invalid: {}", name, e))
        })
        .collect();
    let pattern = RhythmPattern {
        name: name.to_string(),
        bars: built,
    };
    pattern
        .validate()
        .unwrap_or_else(|e| panic!("built-in pattern {} is invalid: {}", name, e));
    pattern
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
    first_bar(&mut damped).mute_ticks = 960;

    // The downbeats at full level, the offbeats under them. This used to be a
    // second, quieter layer, which was the only way an older build could say
    // "quieter"; per-cell velocity says it directly.
    let mut accented = builtin("Accented Eighths", 1.0, "x-x-x-x-");
    for step in [2, 4, 6] {
        first_bar(&mut accented).set_cell_velocity(step, 0.55);
    }

    // Charleston: the dotted quarter is a *length*, not just a long gap — the
    // first hit rings for a dotted quarter and the answer is an eighth.
    let mut charleston = builtin("Charleston", 2.0, "x-----x---------");
    first_bar(&mut charleston).set_cell_hold(0, 1440);

    // Tresillo's 3+3+2 is three lengths, so the pattern carries all three: two
    // dotted quarters and a quarter to close the bar.
    let mut tresillo = builtin("Tresillo", 4.0, "x-----x-----x---");
    first_bar(&mut tresillo).set_cell_hold(0, 1440);
    first_bar(&mut tresillo).set_cell_hold(6, 1440);

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
    // A pattern longer than a bar, which is what the tap looper records. Assigning
    // one spreads its bars across consecutive chords, so a four-bar phrase is one
    // library entry rather than four entries whose names have to be kept in step.
    patterns.push(jazz_chorus());
    patterns.push(son_clave());

    patterns
}

/// A four-bar jazz comp.
///
/// The arc is the one a player actually uses: state the pulse, answer it with a
/// Charleston, add the "&" of 3, then tighten the same figure into a turnaround
/// fill. Sixteenth grid throughout, short holds — comping, not pads.
fn jazz_chorus() -> RhythmPattern {
    builtin_phrase(
        "Jazz Chorus",
        2.0,
        &[
            // 1: beats 1 and 3 — the two-feel statement.
            "x-------x-------",
            // 2: Charleston, the answer.
            "x-----x---------",
            // 3: the same, plus the "&" of 3.
            "x-----x---x-----",
            // 4: the turnaround — two sixteenth pushes before the bar line.
            "x-----x---x-x-x-",
        ],
    )
}

/// The 3-2 son clave over two bars: the three-side, then the two-side.
///
/// The eighth grid keeps the onsets exactly where the clave puts them.
fn son_clave() -> RhythmPattern {
    builtin_phrase("Son Clave", 1.0, &["x--x--x-", "--x-x---"])
}

// -----------------------------------------------------------------------------
// Wire form
// -----------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PatternWire {
    name: String,
    /// The bars, in order. One is the common case.
    ///
    /// A file written before patterns could hold more than one bar has no `bars`
    /// and carries `layers` instead; `TryFrom` folds those into a single bar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bars: Vec<BarWire>,
    /// The old one-bar shape, read only.
    ///
    /// Every field here is the bar's, and every file written before this version
    /// has them at the top level rather than inside a `[[rhythms.bars]]` table.
    #[serde(default = "default_hold", skip_serializing)]
    hold: u32,
    /// Version 1 of this format wrote `gate`, a fraction of a grid cell;
    /// `TryFrom` converts one when it is the only thing present, so an old file
    /// keeps the sound it had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gate: Option<f64>,
    #[serde(default, skip_serializing_if = "is_zero_mute")]
    mute: u32,
    #[serde(default, skip_serializing_if = "all_zero")]
    holds: Vec<u32>,
    #[serde(default, skip_serializing_if = "all_full")]
    velocities: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    swing: Option<f32>,
    /// The take stack, read only — folded into one grid on the way in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    layers: Vec<LayerWire>,
}

/// One bar in the file.
#[derive(Serialize, Deserialize)]
struct BarWire {
    steps: String,
    #[serde(default = "default_hold")]
    hold: u32,
    #[serde(default, skip_serializing_if = "is_zero_mute")]
    mute: u32,
    #[serde(default, skip_serializing_if = "all_zero")]
    holds: Vec<u32>,
    #[serde(default, skip_serializing_if = "all_full")]
    velocities: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    swing: Option<f32>,
}

/// One take in a file written before patterns held bars.
#[derive(Serialize, Deserialize)]
struct LayerWire {
    #[serde(default = "default_gain")]
    gain: f32,
    steps: String,
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

fn default_gain() -> f32 {
    1.0
}

impl From<RhythmPattern> for PatternWire {
    fn from(p: RhythmPattern) -> Self {
        PatternWire {
            name: p.name,
            bars: p
                .bars
                .into_iter()
                .map(|bar| BarWire {
                    steps: bar.to_step_string(),
                    hold: bar.hold,
                    mute: bar.mute_ticks,
                    holds: bar.holds,
                    velocities: bar.velocities,
                    swing: bar.swing,
                })
                .collect(),
            // The one-bar fields are the reader's, never the writer's: a pattern
            // is written as `bars` and nothing else.
            hold: DEFAULT_HOLD,
            gate: None,
            mute: 0,
            holds: Vec::new(),
            velocities: Vec::new(),
            swing: None,
            layers: Vec::new(),
        }
    }
}

impl TryFrom<PatternWire> for RhythmPattern {
    type Error = RhythmError;

    fn try_from(wire: PatternWire) -> Result<Self, RhythmError> {
        let bars = if wire.bars.is_empty() {
            vec![bar_from_legacy(&wire)?]
        } else {
            wire.bars
                .into_iter()
                .map(|bar| {
                    let steps = parse_steps(&bar.steps)?;
                    let built = RhythmBar {
                        steps,
                        holds: bar.holds,
                        velocities: bar.velocities,
                        hold: bar.hold,
                        mute_ticks: bar.mute,
                        swing: bar.swing,
                    };
                    built.validate()?;
                    Ok(built)
                })
                .collect::<Result<Vec<_>, RhythmError>>()?
        };

        let pattern = RhythmPattern {
            name: wire.name,
            bars,
        };
        pattern.validate()?;
        Ok(pattern)
    }
}

/// The one bar an older file describes, from its take stack.
///
/// A file from before patterns held bars has its grid, its holds and its mute at
/// the top level and a `layers` stack inside. The stack is gone, so its steps are
/// **unioned** into one grid — a cell any take hit is a cell that sounds — and the
/// topmost take's gain becomes that cell's accent, which is the nearest a single
/// grid can come to the stack's shape. Losing the stack is the point of the
/// change; losing the *hits* would be losing somebody's performance.
fn bar_from_legacy(wire: &PatternWire) -> Result<RhythmBar, RhythmError> {
    if wire.layers.is_empty() {
        // No bars and no layers: a silent default grid, which is what a file
        // naming a pattern but describing nothing has always meant.
        return Ok(RhythmBar::silent(DEFAULT_STEPS, wire.hold));
    }
    let mut grids = Vec::with_capacity(wire.layers.len());
    for layer in &wire.layers {
        grids.push((parse_steps(&layer.steps)?, layer.gain));
    }
    let steps_len = grids[0].0.len();
    for (grid, _) in &grids {
        if grid.len() != steps_len {
            return Err(RhythmError::ShapeLengthMismatch {
                what: "layers",
                expected: steps_len,
                found: grid.len(),
            });
        }
    }
    let mut steps = vec![false; steps_len];
    let mut velocities: Vec<f32> = vec![1.0; steps_len];
    for (grid, gain) in grids.iter().rev() {
        for (cell, on) in grid.iter().enumerate() {
            if *on {
                steps[cell] = true;
                velocities[cell] = gain.clamp(0.0, 1.0);
            }
        }
    }
    // The converted accent is only worth writing when it is not full level.
    if velocities.iter().all(|v| (*v - 1.0).abs() < f32::EPSILON) {
        velocities.clear();
    }
    let hold = match wire.gate {
        Some(gate) => hold_from_gate(gate as f32, steps_len),
        None => wire.hold,
    };
    let bar = RhythmBar {
        steps,
        holds: wire.holds.clone(),
        velocities,
        hold,
        mute_ticks: wire.mute,
        swing: wire.swing,
    };
    bar.validate()?;
    Ok(bar)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(p.bars[0].steps_per_bar(), 2);
        assert_eq!(p.bars[0].step_ticks(), 1920);
        assert_eq!(p.bars[0].hold_ticks(), 1920, "one cell is a half note");
    }

    #[test]
    fn step_ticks_follow_the_resolution() {
        let quarter = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        assert_eq!(quarter.bars[0].steps_per_bar(), 4);
        assert_eq!(quarter.bars[0].step_ticks(), 960);

        let sixteenth = RhythmPattern::from_step_string("s", 0.5, "xxxxxxxxxxxxxxxx").unwrap();
        assert_eq!(sixteenth.bars[0].steps_per_bar(), 16);
        assert_eq!(sixteenth.bars[0].step_ticks(), 240);

        let sixty_fourth = RhythmPattern::blank("b", 64).unwrap();
        assert_eq!(sixty_fourth.bars[0].step_ticks(), 60);
    }

    // ---- step strings ----

    #[test]
    fn step_strings_round_trip() {
        let text = "x--x--x-x--x-x--";
        let pattern = RhythmPattern::from_step_string("sync", 0.5, text).unwrap();
        assert_eq!(pattern.bars[0].to_step_string(), text);
        assert_eq!(pattern.bars[0].hit_steps(), vec![0, 3, 6, 8, 11, 13]);
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
        assert_eq!(p.bars[0].steps_per_bar(), DEFAULT_STEPS);
        assert!(!p.any_hit());
        assert_eq!(p.bars[0].hit_count(), 0);
        assert_eq!(p.bars[0].hit_steps(), Vec::<usize>::new());
    }

    #[test]
    fn a_pattern_without_bars_is_refused() {
        let p = RhythmPattern {
            name: "empty".into(),
            bars: Vec::new(),
        };
        assert_eq!(p.validate(), Err(RhythmError::EmptyPattern));
    }

    #[test]
    fn a_pattern_longer_than_four_bars_is_refused() {
        // The looper records four, the pool is four deep, and a pattern the
        // transport cannot play is worse than one it refuses.
        let p = RhythmPattern {
            name: "five".into(),
            bars: (0..MAX_PATTERN_BARS + 1)
                .map(|_| RhythmBar::from_step_string(1.0, "x---").unwrap())
                .collect(),
        };
        assert_eq!(p.validate(), Err(RhythmError::TooManyBars(5)));
    }

    #[test]
    fn every_bar_of_a_pattern_must_be_valid() {
        // A phrase is only as good as its worst bar, and a bar whose overrides
        // do not match its grid would silently apply the wrong cell's hold.
        let mut second = RhythmBar::from_step_string(1.0, "x---").unwrap();
        second.holds = vec![0, 0];
        let p = RhythmPattern {
            name: "mixed".into(),
            bars: vec![RhythmBar::from_step_string(1.0, "x---").unwrap(), second],
        };
        assert_eq!(
            p.validate(),
            Err(RhythmError::ShapeLengthMismatch {
                what: "holds",
                expected: 4,
                found: 2
            })
        );
    }

    #[test]
    fn a_hold_outside_the_range_is_refused() {
        let mut p = RhythmPattern::from_step_string("g", 0.5, "x---").unwrap();
        p.bars[0].hold = BAR_TICKS as u32;
        p.validate().unwrap();
        p.bars[0].hold = BAR_TICKS as u32 + 1;
        assert_eq!(
            p.validate(),
            Err(RhythmError::BadHold(BAR_TICKS as u32 + 1)),
            "a hold cannot outlive its bar"
        );
        p.bars[0].hold = 0;
        assert_eq!(p.validate(), Err(RhythmError::BadHold(0)));
    }

    #[test]
    fn a_hold_in_ticks_does_not_move_when_the_grid_does() {
        // The point of measuring ticks: the same pattern on a finer grid still
        // holds a half note.
        let mut p = RhythmPattern::from_step_string("h", 2.0, "x---").unwrap();
        assert_eq!(p.bars[0].hold, 1920);
        // A finer grid is a new bar, which keeps the hold because it is ticks.
        p.bars[0].steps = vec![false; 16];
        assert_eq!(
            p.bars[0].hold, 1920,
            "still a half note on a sixteenth grid"
        );
        assert_eq!(p.bars[0].hold_ticks(), 1920);
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
        assert_eq!(half.bars[0].hold_ticks(), 1920);

        let dotted = RhythmPattern::from_step_string("d", 3.0, "x---").unwrap();
        assert_eq!(dotted.bars[0].hold_ticks(), 2880);

        let whole = RhythmPattern::from_step_string("w", 4.0, "x---").unwrap();
        assert_eq!(whole.bars[0].hold_ticks(), BAR_TICKS);
    }

    #[test]
    fn the_constructor_takes_a_cell_fraction_and_bakes_the_grid_in() {
        // "Two cells" means two of *this* pattern's cells, converted once at
        // construction; the pattern then stores ticks and the grid is irrelevant.
        let coarse = RhythmPattern::from_step_string("c", 2.0, "x---").unwrap();
        let fine = RhythmPattern::from_step_string("f", 2.0, "xxxxxxxxxxxxxxxx").unwrap();
        assert_eq!(coarse.bars[0].hold_ticks(), 1920, "two quarter cells");
        assert_eq!(fine.bars[0].hold_ticks(), 480, "two sixteenth cells");
    }

    #[test]
    fn hold_ticks_can_never_outlive_the_bar() {
        let mut p = RhythmPattern::from_step_string("g", 1.0, "xxxxxxxx").unwrap();
        p.bars[0].hold = 99_999;
        assert_eq!(p.bars[0].hold_ticks(), BAR_TICKS);
    }

    #[test]
    fn a_constructor_clamps_the_gate_to_the_grid() {
        // `from_step_string` sees the grid, so it can clamp rather than refuse:
        // nine cells of a four-cell bar is the whole bar.
        let p = RhythmPattern::from_step_string("g", 9.0, "x---").unwrap();
        assert_eq!(p.bars[0].hold, BAR_TICKS as u32);
        p.validate().unwrap();
    }

    // ---- the muted tail ----

    #[test]
    fn the_mute_boundary_is_the_end_of_the_bar_minus_the_mute() {
        let mut p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        p.bars[0].mute_ticks = 240;
        assert_eq!(p.bars[0].mute_boundary(), Some(BAR_TICKS - 240));
        p.bars[0].mute_ticks = MAX_MUTE_TICKS;
        assert_eq!(p.bars[0].mute_boundary(), Some(BAR_TICKS - 960));
    }

    #[test]
    fn zero_mute_has_no_boundary_at_all() {
        // `None` rather than a boundary on the bar line: a boundary there would
        // still trim a hold that crosses the bar, which is what offsets are for.
        let p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        assert_eq!(p.bars[0].mute_ticks, 0);
        assert_eq!(p.bars[0].mute_boundary(), None);
    }

    #[test]
    fn a_mute_longer_than_a_quarter_note_is_refused() {
        let mut p = RhythmPattern::from_step_string("m", 0.5, "x---").unwrap();
        p.bars[0].mute_ticks = MAX_MUTE_TICKS;
        p.validate().unwrap();
        p.bars[0].mute_ticks = MAX_MUTE_TICKS + 240;
        assert_eq!(
            p.validate(),
            Err(RhythmError::BadMute(MAX_MUTE_TICKS + 240))
        );
    }

    #[test]
    fn a_new_pattern_has_no_mute() {
        assert_eq!(RhythmPattern::draft().bars[0].mute_ticks, 0);
        assert_eq!(RhythmPattern::blank("b", 16).unwrap().bars[0].mute_ticks, 0);
    }

    #[test]
    fn the_mute_survives_a_toml_round_trip() {
        let mut pattern = RhythmPattern::from_step_string("Cut", 0.5, "x---").unwrap();
        pattern.bars[0].mute_ticks = 480;
        assert_eq!(round_trip(pattern.clone()), pattern);
    }

    #[test]
    fn a_pattern_without_a_mute_in_the_file_reads_as_no_mute() {
        // Every rhythms.toml written before this field existed.
        let text = "[[rhythms]]\nname = \"Old\"\ngate = 0.5\n\n[[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].bars[0].mute_ticks, 0);
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
        assert_eq!(pattern.bars[0].hold, 768, "0.8 of a 960-tick cell");
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
        assert_eq!(
            file.rhythms[0].bars[0].hold, 240,
            "half of an eighth-note cell"
        );

        // And the same gate on a coarser grid is a longer hold, as it was.
        let coarse = "[[rhythms]]\nname = \"Old\"\ngate = 0.5\n\n\
                      [[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(coarse).unwrap();
        assert_eq!(file.rhythms[0].bars[0].hold, 480);
    }

    #[test]
    fn the_quarters_built_in_is_four_quarter_notes() {
        // It used to be `x---`: one chord per bar under a name that means four
        // quarter notes.
        let quarters = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "Quarters")
            .expect("Quarters");
        assert_eq!(quarters.bars[0].steps_per_bar(), 4);
        assert_eq!(quarters.bars[0].hit_steps(), vec![0, 1, 2, 3]);
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
        assert_eq!(
            half.bars[0].hit_steps(),
            vec![0],
            "one hit, on the downbeat"
        );
        assert_eq!(half.bars[0].hold_ticks(), 1920, "a half note");

        let dotted = held("Held 3/4");
        assert_eq!(dotted.bars[0].hit_steps(), vec![0]);
        assert_eq!(dotted.bars[0].hold_ticks(), 2880, "a dotted half");
    }

    #[test]
    fn hold_ticks_are_one_step_times_the_gate() {
        let p = RhythmPattern::from_step_string("q", 1.0, "x---").unwrap();
        assert_eq!(p.bars[0].hold_ticks(), 960);
        let p = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        assert_eq!(p.bars[0].hold_ticks(), 480);
        // Past one cell, which is how a held half note is written.
        let p = RhythmPattern::from_step_string("h", 2.0, "x---").unwrap();
        assert_eq!(p.bars[0].hold_ticks(), 1920);
        // Always at least one tick, so a zero-length note never reaches the
        // writer.
        let mut tiny = RhythmPattern::from_step_string("q", 0.5, "x---").unwrap();
        tiny.bars[0].hold = 0;
        assert_eq!(tiny.bars[0].hold_ticks(), 1);
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
    fn a_one_bar_pattern_round_trips_through_toml() {
        let pattern = RhythmPattern::single(
            "Offbeat",
            RhythmBar::from_step_string(1.0, "x--x--x-").unwrap(),
        );
        assert_eq!(round_trip(pattern.clone()), pattern);
    }

    #[test]
    fn a_four_bar_pattern_round_trips_through_toml() {
        // What the tap looper produces: one library entry holding four bars,
        // rather than four entries whose names have to agree.
        let pattern = RhythmPattern {
            name: "Jazz Chorus".into(),
            bars: [
                "x-------x-------",
                "x-----x---------",
                "x-----x---x-----",
                "x-----x---x-x-x-",
            ]
            .iter()
            .map(|steps| RhythmBar::from_step_string(2.0, steps).unwrap())
            .collect(),
        };
        let back = round_trip(pattern.clone());
        assert_eq!(back, pattern);
        assert_eq!(back.bars.len(), MAX_PATTERN_BARS);
        assert_eq!(back.bars[3].hit_steps(), vec![0, 6, 10, 12, 14]);
    }

    #[test]
    fn a_file_of_layers_reads_as_one_grid() {
        // The shape every rhythms.toml written before patterns held bars has.
        // The stack is gone, so its steps are unioned: a cell any take hit is a
        // cell that sounds, and the topmost take's gain becomes the accent.
        let text = "[[rhythms]]\nname = \"Old\"\ngate = 1.0\n\n\
                    [[rhythms.layers]]\ngain = 1.0\nsteps = \"x---\"\n\n\
                    [[rhythms.layers]]\ngain = 0.5\nsteps = \"--x-\"\n";
        let file: File = toml::from_str(text).unwrap();
        let bar = &file.rhythms[0].bars[0];
        assert_eq!(bar.step_ticks(), 960, "the grid is the takes' own");
        assert!(bar.hit(0) && bar.hit(2), "both takes' hits survive");
        assert_eq!(bar.cell_velocity(0), 1.0, "the top take's level");
        assert_eq!(bar.cell_velocity(2), 0.5, "and the one under it");
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
    fn a_pattern_written_with_no_bars_at_all_still_reads() {
        // The one-bar fields are the reader's: a file that names a pattern and
        // describes nothing is a silent grid, which is what it has always meant.
        let text = "[[rhythms]]\nname = \"x\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].bars.len(), 1);
        assert!(!file.rhythms[0].any_hit());
    }

    #[test]
    fn a_file_with_more_bars_than_are_allowed_is_refused() {
        let text = "[[rhythms]]\nname = \"long\"\n\n\
                    [[rhythms.bars]]\nsteps = \"x---\"\n\n\
                    [[rhythms.bars]]\nsteps = \"x---\"\n\n\
                    [[rhythms.bars]]\nsteps = \"x---\"\n\n\
                    [[rhythms.bars]]\nsteps = \"x---\"\n\n\
                    [[rhythms.bars]]\nsteps = \"x---\"\n";
        assert!(toml::from_str::<File>(text).is_err());
    }

    #[test]
    fn a_bar_defaults_to_the_default_hold() {
        let text = "[[rhythms]]\nname = \"Bare\"\n\n[[rhythms.layers]]\nsteps = \"x---\"\n";
        let file: File = toml::from_str(text).unwrap();
        assert_eq!(file.rhythms[0].bars[0].hold, DEFAULT_HOLD);
        assert_eq!(file.rhythms[0].bars[0].steps_per_bar(), 4);
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
        assert_eq!(patterns.len(), 19, "the shipped palette");
        let mut names: Vec<&str> = patterns.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), patterns.len(), "duplicate built-in name");
        for pattern in &patterns {
            assert!(pattern.any_hit(), "{} is silent", pattern.name);
        }
    }

    #[test]
    fn the_phrases_hold_their_bars_in_one_pattern() {
        // A phrase used to be a *set* of one-bar patterns whose names carried the
        // position — `Jazz Chorus 1/4` and so on — so the numbering was the
        // feature and a missing bar played the figure wrong. It is one pattern
        // with four bars now, which is the same guarantee without the names.
        let patterns = builtin_patterns();
        let phrase = |name: &str| {
            patterns
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{} is missing", name))
                .clone()
        };
        let jazz = phrase("Jazz Chorus");
        assert_eq!(jazz.bars.len(), 4);
        assert_eq!(jazz.bars[0].hit_steps(), vec![0, 8]);
        assert_eq!(jazz.bars[1].hit_steps(), vec![0, 6]);
        assert_eq!(jazz.bars[2].hit_steps(), vec![0, 6, 10]);
        assert_eq!(jazz.bars[3].hit_steps(), vec![0, 6, 10, 12, 14]);

        let clave = phrase("Son Clave");
        assert_eq!(clave.bars.len(), 2);
        assert_eq!(clave.bars[0].hit_steps(), vec![0, 3, 6]);
        assert_eq!(clave.bars[1].hit_steps(), vec![2, 4]);

        // And no pattern is named for a bar of another any more: the count is
        // the guarantee, because a stray numbered bar would show up as a
        // twentieth entry.
        assert_eq!(patterns.len(), 19, "a numbered bar would be one more");
    }

    #[test]
    fn the_offbeat_built_in_is_actually_syncopated() {
        // The feature's whole point: hits that are not on the beat.
        let patterns = builtin_patterns();
        let offbeat = patterns
            .iter()
            .find(|p| p.name == "Offbeat Eighths")
            .expect("Offbeat Eighths");
        assert_eq!(offbeat.bars[0].steps_per_bar(), 8);
        assert_eq!(offbeat.bars[0].hit_steps(), vec![1, 3, 5, 7]);
        assert!(
            !offbeat.bars[0].hit(0),
            "an offbeat pattern has no downbeat"
        );
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
            (pattern.bars[0].steps_per_bar(), pattern.bars[0].hit_steps())
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
    }

    #[test]
    fn the_phrases_have_the_onsets_their_names_claim() {
        let patterns = builtin_patterns();
        let phrase = |name: &str| {
            patterns
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no built-in named {:?}", name))
                .clone()
        };

        // The four-bar jazz comp: pulse, Charleston, add the "&" of 3, fill.
        let jazz = phrase("Jazz Chorus");
        let steps: Vec<Vec<usize>> = jazz.bars.iter().map(|b| b.hit_steps()).collect();
        assert_eq!(
            steps,
            vec![
                vec![0, 8],
                vec![0, 6],
                vec![0, 6, 10],
                vec![0, 6, 10, 12, 14]
            ]
        );
        // 3-2 son clave: the three-side, then the two-side.
        let clave = phrase("Son Clave");
        let sides: Vec<Vec<usize>> = clave.bars.iter().map(|b| b.hit_steps()).collect();
        assert_eq!(sides, vec![vec![0, 3, 6], vec![2, 4]]);
    }

    #[test]
    fn held_whole_is_the_longest_hold_there_is() {
        let pattern = builtin_patterns()
            .into_iter()
            .find(|p| p.name == "Held Whole")
            .expect("Held Whole");
        assert_eq!(
            pattern.bars[0].hold_ticks(),
            BAR_TICKS,
            "one hit, the whole bar"
        );
        assert_eq!(pattern.bars[0].hit_steps(), vec![0]);
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
            damped.bars[0].mute_boundary(),
            Some(BAR_TICKS - MAX_MUTE_TICKS as u64)
        );
        assert_eq!(damped.bars[0].hold_ticks(), 384, "damped, not ringing");

        let accented = patterns
            .iter()
            .find(|p| p.name == "Accented Eighths")
            .expect("Accented Eighths");
        assert_eq!(
            accented.bars[0].cell_velocity(0),
            1.0,
            "the downbeat is full"
        );
        for step in [2, 4, 6] {
            assert_eq!(
                accented.bars[0].cell_velocity(step),
                0.55,
                "the offbeats sit under"
            );
            assert!(accented.bars[0].has_velocity_override(step));
        }
        assert!(
            !accented.bars[0].has_velocity_override(0),
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
        assert_eq!(charleston.bars[0].hold, 480, "the answer is an eighth");
        assert_eq!(
            charleston.bars[0].cell_hold(0),
            1440,
            "the statement is a dotted quarter"
        );
        assert_eq!(
            charleston.bars[0].cell_hold(6),
            480,
            "and the default holds for the rest"
        );

        let tresillo = find("Tresillo");
        assert_eq!(tresillo.bars[0].cell_hold(0), 1440);
        assert_eq!(tresillo.bars[0].cell_hold(6), 1440);
        assert_eq!(
            tresillo.bars[0].cell_hold(12),
            960,
            "3+3+2 closes with a quarter"
        );
    }

    // ---- per-cell shape ----

    #[test]
    fn a_cell_override_round_trips_and_clears() {
        let mut pattern = RhythmPattern::from_step_string("Shape", 1.0, "x-x-").unwrap();
        assert!(!pattern.bars[0].has_hold_override(0));
        assert_eq!(
            pattern.bars[0].cell_hold(0),
            pattern.bars[0].hold,
            "the default, until set"
        );

        pattern.bars[0].set_cell_hold(0, 1440);
        assert!(pattern.bars[0].has_hold_override(0));
        assert_eq!(pattern.bars[0].cell_hold(0), 1440);
        assert_eq!(
            pattern.bars[0].holds.len(),
            4,
            "one entry per cell once used"
        );
        assert_eq!(
            pattern.bars[0].cell_hold(2),
            pattern.bars[0].hold,
            "only the one cell moved"
        );

        // Setting it back to the default *clears* the override, so the pattern
        // does not carry a row of numbers saying nothing.
        let default = pattern.bars[0].hold;
        pattern.bars[0].set_cell_hold(0, default);
        assert!(!pattern.bars[0].has_hold_override(0));
        assert!(pattern.bars[0].holds.is_empty(), "and the array is dropped");
    }

    #[test]
    fn a_hold_marks_the_cells_it_is_still_sounding_through() {
        // What the grid draws: an onset is a hit, and the cells its hold covers
        // are the line after it. Without this a note held for a bar and a stab
        // look identical on screen.
        let mut bar = RhythmPattern::from_step_string("Held", 1.0, "x---")
            .unwrap()
            .bars[0]
            .clone();
        assert_eq!(bar.step_ticks(), 960, "four cells to the bar");
        assert!(
            bar.sustain().iter().all(|held| !held),
            "a one-cell hold leaves nothing to draw"
        );

        bar.set_cell_hold(0, 2880);
        assert_eq!(
            bar.sustain(),
            vec![false, true, true, false],
            "three cells of hold covers the two after the onset"
        );

        // A hold running off the end of the bar stops at the bar line: the next
        // bar has its own grid and its own hits, so there is nothing to mark.
        let mut late = RhythmBar::silent(4, 960);
        late.steps[2] = true;
        late.set_cell_hold(2, BAR_TICKS as u32);
        assert_eq!(
            late.sustain(),
            vec![false, false, false, true],
            "clipped at the bar, not wrapped"
        );
    }

    #[test]
    fn accents_and_lengths_survive_the_wire() {
        let mut pattern = RhythmPattern::from_step_string("Shaped", 1.0, "x-x-").unwrap();
        pattern.bars[0].set_cell_hold(0, 1440);
        pattern.bars[0].set_cell_velocity(2, 0.5);
        pattern.bars[0].swing = Some(0.4);

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
        pattern.bars[0].holds = vec![1440, 0];
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
            pattern.bars[0].swing_or(0.6),
            0.6,
            "no override follows the transport"
        );
        pattern.bars[0].swing = Some(0.2);
        assert_eq!(pattern.bars[0].swing_or(0.6), 0.2, "an override wins");

        pattern.bars[0].swing = Some(4.0);
        assert!(pattern.validate().is_err(), "and it is still range-checked");
    }
}

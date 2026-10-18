//! The decoder's budget and statistics (M3 §10.6).
//!
//! Every count the decoder keeps is bounded by a [`TsBudget`] field, and
//! every check happens before the work it guards. [`TsStats`] reports what
//! one decode used, so tests can assert the bounds held, on success and on
//! failure alike.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use super::{TsError, TsErrorKind};

/// Limits for one typedstream decode.
///
/// Construct it with [`TsBudget::ceiling`] (or `Default`, which is the
/// same in M3) and tighten it with the `with_*` setters. Every setter
/// clamps its value into `1..=` the ceiling's value, so no constructible
/// budget is looser than the ceiling and no caller can turn a limit off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TsBudget {
    max_input: u32,
    max_depth: u16,
    max_objects: u32,
    max_shared: u32,
    max_nodes: u32,
    max_runs: u32,
    materialised_factor: u32,
    materialised_floor: u32,
}

const CEILING: TsBudget = TsBudget {
    max_input: 16 << 20,
    max_depth: 64,
    max_objects: 262_144,
    max_shared: 65_536,
    max_nodes: 1_048_576,
    max_runs: 1_048_576,
    materialised_factor: 2,
    materialised_floor: 64 << 10,
};

const fn clamp32(n: u32, max: u32) -> u32 {
    if n == 0 {
        1
    } else if n > max {
        max
    } else {
        n
    }
}

const fn clamp16(n: u16, max: u16) -> u16 {
    if n == 0 {
        1
    } else if n > max {
        max
    } else {
        n
    }
}

impl TsBudget {
    /// The budget M3 decodes with.
    pub const DEFAULT: TsBudget = CEILING;

    /// The loosest budget: in M3, equal to [`TsBudget::DEFAULT`].
    pub const fn ceiling() -> Self {
        CEILING
    }

    /// The largest input, in bytes (16 MiB).
    pub const fn with_max_input(mut self, n: u32) -> Self {
        self.max_input = clamp32(n, CEILING.max_input);
        self
    }

    /// The deepest nesting of objects plus structs (64).
    pub const fn with_max_depth(mut self, n: u16) -> Self {
        self.max_depth = clamp16(n, CEILING.max_depth);
        self
    }

    /// The most object-table slots: objects, classes and `char *`
    /// pointers (262,144).
    pub const fn with_max_objects(mut self, n: u32) -> Self {
        self.max_objects = clamp32(n, CEILING.max_objects);
        self
    }

    /// The most shared strings (65,536).
    pub const fn with_max_shared(mut self, n: u32) -> Self {
        self.max_shared = clamp32(n, CEILING.max_shared);
        self
    }

    /// The most type elements read, zero-width ones included (1,048,576).
    pub const fn with_max_nodes(mut self, n: u32) -> Self {
        self.max_nodes = clamp32(n, CEILING.max_nodes);
        self
    }

    /// The most attribute runs (1,048,576).
    pub const fn with_max_runs(mut self, n: u32) -> Self {
        self.max_runs = clamp32(n, CEILING.max_runs);
        self
    }

    /// Bytes that may be copied out per input byte (2).
    pub const fn with_materialised_factor(mut self, n: u32) -> Self {
        self.materialised_factor = clamp32(n, CEILING.materialised_factor);
        self
    }

    /// Bytes that may be copied out whatever the input's size (64 KiB).
    pub const fn with_materialised_floor(mut self, n: u32) -> Self {
        self.materialised_floor = clamp32(n, CEILING.materialised_floor);
        self
    }

    /// The largest input, in bytes.
    pub const fn max_input(&self) -> u32 {
        self.max_input
    }

    /// The deepest nesting.
    pub const fn max_depth(&self) -> u16 {
        self.max_depth
    }

    /// The most object-table slots.
    pub const fn max_objects(&self) -> u32 {
        self.max_objects
    }

    /// The most shared strings.
    pub const fn max_shared(&self) -> u32 {
        self.max_shared
    }

    /// The most type elements read.
    pub const fn max_nodes(&self) -> u32 {
        self.max_nodes
    }

    /// The most runs.
    pub const fn max_runs(&self) -> u32 {
        self.max_runs
    }

    /// The most bytes a decode of `input_len` bytes may copy out:
    /// `factor × input_len + floor`.
    pub const fn max_materialised_bytes(&self, input_len: usize) -> u64 {
        let len = input_len as u64;
        len.saturating_mul(self.materialised_factor as u64)
            .saturating_add(self.materialised_floor as u64)
    }
}

impl Default for TsBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What one decode used. Every field stays within the matching
/// [`TsBudget`] limit, whether the decode succeeded or failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TsStats {
    /// Type elements read (one per value, plus one per struct, zero-width
    /// elements included).
    pub nodes: u32,
    /// Object-table slots created.
    pub slots: u32,
    /// Shared strings created.
    pub shared: u32,
    /// Attribute runs read.
    pub runs: u32,
    /// Bytes copied out of the input: text, attribute strings, run and
    /// part records.
    pub bytes_materialised: u64,
    /// The deepest nesting of objects plus structs reached.
    pub max_depth_seen: u16,
    /// Type strings parsed. A type string is parsed once however often it
    /// is referenced.
    pub type_parses: u32,
}

/// The running account of one decode.
pub(crate) struct Meter {
    budget: TsBudget,
    materialised_limit: u64,
    stats: TsStats,
}

fn fail(kind: TsErrorKind, offset: usize) -> TsError {
    TsError::new(kind, offset)
}

impl Meter {
    pub(crate) fn new(budget: &TsBudget, input_len: usize) -> Self {
        Meter {
            budget: *budget,
            materialised_limit: budget.max_materialised_bytes(input_len),
            stats: TsStats::default(),
        }
    }

    pub(crate) fn budget(&self) -> &TsBudget {
        &self.budget
    }

    pub(crate) fn stats(&self) -> TsStats {
        self.stats
    }

    /// Charge `n` type elements.
    pub(crate) fn nodes(&mut self, n: u32, at: usize) -> Result<(), TsError> {
        match self.stats.nodes.checked_add(n) {
            Some(total) if total <= self.budget.max_nodes => {
                self.stats.nodes = total;
                Ok(())
            }
            _ => Err(fail(TsErrorKind::Budget, at)),
        }
    }

    pub(crate) fn slot(&mut self, at: usize) -> Result<(), TsError> {
        if self.stats.slots >= self.budget.max_objects {
            return Err(fail(TsErrorKind::Budget, at));
        }
        self.stats.slots = self.stats.slots.saturating_add(1);
        Ok(())
    }

    pub(crate) fn shared(&mut self, at: usize) -> Result<(), TsError> {
        if self.stats.shared >= self.budget.max_shared {
            return Err(fail(TsErrorKind::Budget, at));
        }
        self.stats.shared = self.stats.shared.saturating_add(1);
        Ok(())
    }

    pub(crate) fn run(&mut self, at: usize) -> Result<(), TsError> {
        if self.stats.runs >= self.budget.max_runs {
            return Err(fail(TsErrorKind::Budget, at));
        }
        self.stats.runs = self.stats.runs.saturating_add(1);
        Ok(())
    }

    pub(crate) fn type_parsed(&mut self) {
        self.stats.type_parses = self.stats.type_parses.saturating_add(1);
    }

    /// Record a nesting depth; past the budget it is `TooDeep`.
    pub(crate) fn depth(&mut self, depth: u32, at: usize) -> Result<(), TsError> {
        if depth > u32::from(self.budget.max_depth) {
            return Err(fail(TsErrorKind::TooDeep, at));
        }
        let d = u16::try_from(depth).unwrap_or(u16::MAX);
        if d > self.stats.max_depth_seen {
            self.stats.max_depth_seen = d;
        }
        Ok(())
    }

    /// Charge `bytes` copied out, before they are copied.
    pub(crate) fn materialise(&mut self, bytes: u64, at: usize) -> Result<(), TsError> {
        match self.stats.bytes_materialised.checked_add(bytes) {
            Some(total) if total <= self.materialised_limit => {
                self.stats.bytes_materialised = total;
                Ok(())
            }
            _ => Err(fail(TsErrorKind::MaterialisationBudget, at)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setters_clamp_into_one_to_ceiling() {
        let c = TsBudget::ceiling();
        assert_eq!(TsBudget::default(), c);
        assert_eq!(TsBudget::DEFAULT, c);
        let loose = c
            .with_max_input(u32::MAX)
            .with_max_depth(u16::MAX)
            .with_max_objects(u32::MAX)
            .with_max_shared(u32::MAX)
            .with_max_nodes(u32::MAX)
            .with_max_runs(u32::MAX)
            .with_materialised_factor(u32::MAX)
            .with_materialised_floor(u32::MAX);
        assert_eq!(loose, c);
        let zero = c
            .with_max_input(0)
            .with_max_depth(0)
            .with_max_objects(0)
            .with_max_shared(0)
            .with_max_nodes(0)
            .with_max_runs(0)
            .with_materialised_factor(0)
            .with_materialised_floor(0);
        assert_eq!(zero.max_input(), 1);
        assert_eq!(zero.max_depth(), 1);
        assert_eq!(zero.max_objects(), 1);
        assert_eq!(zero.max_shared(), 1);
        assert_eq!(zero.max_nodes(), 1);
        assert_eq!(zero.max_runs(), 1);
        assert_eq!(zero.max_materialised_bytes(10), 11);
        assert_eq!(c.max_materialised_bytes(100), 200 + 65_536);
        assert_eq!(c.max_input(), 16 << 20);
        assert_eq!(c.max_depth(), 64);
    }

    #[test]
    fn meter_refuses_before_passing_a_limit() {
        let b = TsBudget::ceiling().with_max_nodes(3).with_max_runs(1);
        let mut m = Meter::new(&b, 0);
        assert!(m.nodes(2, 0).is_ok());
        assert_eq!(
            m.nodes(2, 7).map_err(|e| e.kind()),
            Err(TsErrorKind::Budget)
        );
        assert!(m.nodes(1, 0).is_ok());
        assert!(m.run(0).is_ok());
        assert!(m.run(0).is_err());
        assert!(m.depth(64, 0).is_ok());
        assert_eq!(
            m.depth(65, 0).map_err(|e| e.kind()),
            Err(TsErrorKind::TooDeep)
        );
        assert!(m.materialise(65_536, 0).is_ok());
        assert_eq!(
            m.materialise(1, 0).map_err(|e| e.kind()),
            Err(TsErrorKind::MaterialisationBudget)
        );
        let s = m.stats();
        assert_eq!((s.nodes, s.runs, s.max_depth_seen), (3, 1, 64));
    }
}

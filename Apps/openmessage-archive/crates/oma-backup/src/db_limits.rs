//! `sqlite3_limit` values for a staged database (sqlite.org/security.html).
//!
//! A caller can only tighten these limits, never loosen them: every setter
//! clamps its value into `1..=ceiling`, where the ceiling is the set the
//! crate has always used for staged databases.

use rusqlite::limits::Limit as SqlLimit;
use rusqlite::Connection;

/// Numeric SQLite limits for one hardened connection to a staged database.
///
/// Construct it with [`StagedDbLimits::ceiling`] and tighten it with the
/// `with_*` setters. Every value stays within `1..=` the ceiling's value, so
/// no constructible value is looser than the ceiling. The non-numeric
/// hardening (`DEFENSIVE`, `query_only`, no `ATTACH`, a trigger depth of 1,
/// double-quoted strings, triggers and views off, and the rest) is not
/// configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StagedDbLimits {
    length: i32,
    sql_length: i32,
    expr_depth: i32,
    column: i32,
    vdbe_op: i32,
    function_arg: i32,
    like_pattern_length: i32,
    variable_number: i32,
}

/// The loosest limits any staged database gets. Wider than Manifest.db's:
/// sms.db's `message` table has many columns and bodies can be long.
const CEILING: StagedDbLimits = StagedDbLimits {
    length: 64 << 20,
    sql_length: 1 << 16,
    expr_depth: 100,
    column: 2000,
    vdbe_op: 250_000,
    function_arg: 32,
    like_pattern_length: 50,
    variable_number: 999,
};

/// Clamp `n` into `1..=max` (`max` is at least 1).
const fn clamp(n: u32, max: i32) -> i32 {
    if n == 0 {
        1
    } else if n >= max as u32 {
        max
    } else {
        // n < max <= i32::MAX, so this cannot wrap.
        n as i32
    }
}

impl StagedDbLimits {
    /// The ceiling: the loosest values a staged database is ever opened
    /// with.
    pub const fn ceiling() -> Self {
        CEILING
    }

    /// `SQLITE_LIMIT_LENGTH`: the largest string or blob, in bytes.
    pub const fn with_length(mut self, n: u32) -> Self {
        self.length = clamp(n, CEILING.length);
        self
    }

    /// `SQLITE_LIMIT_SQL_LENGTH`: the longest SQL statement, schema SQL
    /// included.
    pub const fn with_sql_length(mut self, n: u32) -> Self {
        self.sql_length = clamp(n, CEILING.sql_length);
        self
    }

    /// `SQLITE_LIMIT_EXPR_DEPTH`.
    pub const fn with_expr_depth(mut self, n: u32) -> Self {
        self.expr_depth = clamp(n, CEILING.expr_depth);
        self
    }

    /// `SQLITE_LIMIT_COLUMN`.
    pub const fn with_column(mut self, n: u32) -> Self {
        self.column = clamp(n, CEILING.column);
        self
    }

    /// `SQLITE_LIMIT_VDBE_OP`: compiled program size, not steps executed.
    pub const fn with_vdbe_op(mut self, n: u32) -> Self {
        self.vdbe_op = clamp(n, CEILING.vdbe_op);
        self
    }

    /// `SQLITE_LIMIT_FUNCTION_ARG`.
    pub const fn with_function_arg(mut self, n: u32) -> Self {
        self.function_arg = clamp(n, CEILING.function_arg);
        self
    }

    /// `SQLITE_LIMIT_LIKE_PATTERN_LENGTH`.
    pub const fn with_like_pattern_length(mut self, n: u32) -> Self {
        self.like_pattern_length = clamp(n, CEILING.like_pattern_length);
        self
    }

    /// `SQLITE_LIMIT_VARIABLE_NUMBER`.
    pub const fn with_variable_number(mut self, n: u32) -> Self {
        self.variable_number = clamp(n, CEILING.variable_number);
        self
    }

    /// Every value, in a fixed order: length, sql_length, expr_depth,
    /// column, vdbe_op, function_arg, like_pattern_length, variable_number.
    pub const fn values(&self) -> [u32; 8] {
        // Every field is in 1..=i32::MAX, so the casts are lossless.
        [
            self.length as u32,
            self.sql_length as u32,
            self.expr_depth as u32,
            self.column as u32,
            self.vdbe_op as u32,
            self.function_arg as u32,
            self.like_pattern_length as u32,
            self.variable_number as u32,
        ]
    }

    /// Apply the limits, plus the fixed ones (no `ATTACH`, trigger depth 1).
    pub(crate) fn apply(&self, conn: &Connection) -> rusqlite::Result<()> {
        conn.set_limit(SqlLimit::SQLITE_LIMIT_LENGTH, self.length)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_SQL_LENGTH, self.sql_length)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_ATTACHED, 0)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_EXPR_DEPTH, self.expr_depth)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_COLUMN, self.column)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_VDBE_OP, self.vdbe_op)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_FUNCTION_ARG, self.function_arg)?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_TRIGGER_DEPTH, 1)?;
        conn.set_limit(
            SqlLimit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
            self.like_pattern_length,
        )?;
        conn.set_limit(SqlLimit::SQLITE_LIMIT_VARIABLE_NUMBER, self.variable_number)?;
        Ok(())
    }
}

/// Manifest.db: five known columns and fixed, simple queries.
pub(crate) const MANIFEST_LIMITS: StagedDbLimits = StagedDbLimits::ceiling()
    .with_length(1 << 20)
    .with_column(100)
    .with_vdbe_op(25_000)
    .with_function_arg(8)
    .with_variable_number(10);

#[cfg(test)]
mod tests {
    use super::*;

    type Setter = fn(StagedDbLimits, u32) -> StagedDbLimits;

    const SETTERS: [Setter; 8] = [
        StagedDbLimits::with_length,
        StagedDbLimits::with_sql_length,
        StagedDbLimits::with_expr_depth,
        StagedDbLimits::with_column,
        StagedDbLimits::with_vdbe_op,
        StagedDbLimits::with_function_arg,
        StagedDbLimits::with_like_pattern_length,
        StagedDbLimits::with_variable_number,
    ];

    fn within_ceiling(l: &StagedDbLimits) {
        let ceiling = StagedDbLimits::ceiling().values();
        for (v, c) in l.values().iter().zip(ceiling) {
            assert!(*v >= 1 && *v <= c, "{v} not in 1..={c}");
        }
    }

    #[test]
    fn the_ceiling_is_the_m2_staged_limits() {
        assert_eq!(
            StagedDbLimits::ceiling().values(),
            [64 << 20, 1 << 16, 100, 2000, 250_000, 32, 50, 999]
        );
        assert_eq!(
            MANIFEST_LIMITS.values(),
            [1 << 20, 1 << 16, 100, 100, 25_000, 8, 50, 10]
        );
    }

    #[test]
    fn every_setter_clamps_into_one_to_the_ceiling() {
        let probes = [
            0,
            1,
            2,
            7,
            50,
            999,
            1000,
            65_536,
            250_001,
            i32::MAX as u32,
            i32::MAX as u32 + 1,
            u32::MAX,
        ];
        // Single setters, and every setter applied in sequence with each
        // probe, from the ceiling and from an already tightened value.
        for (i, set) in SETTERS.iter().enumerate() {
            for n in probes {
                let l = set(StagedDbLimits::ceiling(), n);
                within_ceiling(&l);
                let expected = if n == 0 {
                    1
                } else {
                    n.min(StagedDbLimits::ceiling().values()[i])
                };
                assert_eq!(l.values()[i], expected, "setter {i}, value {n}");
            }
        }
        let mut l = StagedDbLimits::ceiling();
        for (k, n) in probes.iter().cycle().take(64).enumerate() {
            l = SETTERS[k % SETTERS.len()](l, *n);
            within_ceiling(&l);
        }
        // Loosening an already tight value is clamped back to the ceiling.
        let tight = StagedDbLimits::ceiling().with_column(10);
        assert_eq!(tight.with_column(u32::MAX).values()[3], 2000);
    }
}

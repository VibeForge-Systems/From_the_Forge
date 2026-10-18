//! `Col<T>`: one source field (M3 §5.2, D49).
//!
//! "The column does not exist", "the value is SQL NULL", "the value is there
//! but unreadable" and "the value is over its cap and was not read" are
//! different facts, and M4 must be able to tell them apart. A plain
//! `Option<T>` would merge them.

/// One source field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Col<T> {
    /// The column does not exist in this database.
    Absent,
    /// SQL NULL.
    Null,
    /// A readable value.
    Value(T),
    /// The column exists but its value cannot be read as the expected type.
    Unreadable(IssueKind),
    /// The value is over the column's cap and was never materialised.
    Oversized {
        /// Its size in bytes, as SQLite reports it (`octet_length`).
        bytes: u64,
    },
}

/// Why a present value is unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IssueKind {
    /// The storage class does not fit the column's kind (for example text in
    /// an integer column, or a real that is not an integral `i64`).
    TypeMismatch,
    /// A boolean column holds an integer other than 0 or 1.
    BoolOutOfRange,
    /// A property-list column (`message_summary_info`) is not a valid
    /// property list.
    PlistInvalid,
    /// A property list went over a reading budget (events, bytes, depth or
    /// array length).
    PlistOverBudget,
    /// A property list whose root is not a dictionary.
    PlistNotDictionary,
}

/// The shape of a [`Col`], without its value. Records print shapes from
/// `Debug`, never values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColShape {
    /// [`Col::Absent`].
    Absent,
    /// [`Col::Null`].
    Null,
    /// [`Col::Value`].
    Value,
    /// [`Col::Unreadable`].
    Unreadable(IssueKind),
    /// [`Col::Oversized`].
    Oversized {
        /// The size in bytes.
        bytes: u64,
    },
}

impl<T> Col<T> {
    /// The value, when there is a readable one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Col::Value(v) => Some(v),
            _ => None,
        }
    }

    /// `true` when the column exists in this database, whatever its value.
    /// A counter that depends on an unrecorded column is "not recorded",
    /// never 0 (I5).
    pub fn is_recorded(&self) -> bool {
        !matches!(self, Col::Absent)
    }

    /// The shape, without the value.
    pub fn shape(&self) -> ColShape {
        match self {
            Col::Absent => ColShape::Absent,
            Col::Null => ColShape::Null,
            Col::Value(_) => ColShape::Value,
            Col::Unreadable(k) => ColShape::Unreadable(*k),
            Col::Oversized { bytes } => ColShape::Oversized { bytes: *bytes },
        }
    }

    /// Map the value, keeping every other case.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Col<U> {
        match self {
            Col::Absent => Col::Absent,
            Col::Null => Col::Null,
            Col::Value(v) => Col::Value(f(v)),
            Col::Unreadable(k) => Col::Unreadable(k),
            Col::Oversized { bytes } => Col::Oversized { bytes },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_null_unreadable_and_oversized_stay_apart() {
        let cases: [Col<i64>; 5] = [
            Col::Absent,
            Col::Null,
            Col::Value(7),
            Col::Unreadable(IssueKind::TypeMismatch),
            Col::Oversized { bytes: 9 },
        ];
        let recorded: Vec<bool> = cases.iter().map(Col::is_recorded).collect();
        assert_eq!(recorded, [false, true, true, true, true]);
        let values: Vec<Option<&i64>> = cases.iter().map(Col::value).collect();
        assert_eq!(values, [None, None, Some(&7), None, None]);
        let shapes: Vec<ColShape> = cases.iter().map(Col::shape).collect();
        assert_eq!(
            shapes,
            [
                ColShape::Absent,
                ColShape::Null,
                ColShape::Value,
                ColShape::Unreadable(IssueKind::TypeMismatch),
                ColShape::Oversized { bytes: 9 }
            ]
        );
        assert_eq!(Col::Value(2).map(|v| v * 3), Col::Value(6));
        assert_eq!(Col::<i64>::Null.map(|v| v * 3), Col::Null);
    }
}

//! Classifying the `sql` text of a `sqlite_schema` row without SQLite.
//!
//! When SQLite loads a schema it hands each row's `sql` to its full
//! tokenizer, which skips any amount of whitespace and both comment forms
//! (`-- …` to the end of the line, `/* … */`) between keywords. It does not
//! consult `rootpage` to decide whether a row is a virtual table. A check
//! that reads only a fixed-length prefix of the text, or requires single
//! spaces between keywords, can therefore be bypassed by a crafted schema
//! (`CREATE/**/VIRTUAL TABLE …`, or padding after `CREATE`).
//!
//! The rule here reads tokens the way SQLite's tokenizer separates them
//! (sqlite.org/lang_comment.html; whitespace per the tokenizer's character
//! classes) and errs towards "virtual": any statement whose second token is
//! spelled `VIRTUAL`, quoted or not, counts. A row SQLite would reject is
//! harmless to over-count, because such a database is refused anyway.
//!
//! The text is only classified. It is never stored, echoed or logged.

/// Whether a `sqlite_schema.sql` value declares a virtual table: its first
/// token is `CREATE` and its second is `VIRTUAL` (ASCII case-insensitive),
/// with any whitespace and comments between and before them.
pub fn sql_declares_virtual_table(sql: &[u8]) -> bool {
    let mut t = Tokens { s: sql, i: 0 };
    t.next().is_some_and(|w| w.eq_ignore_ascii_case(b"CREATE"))
        && t.next()
            .is_some_and(|w| unquote(w).eq_ignore_ascii_case(b"VIRTUAL"))
}

/// Strip one pair of SQL identifier or string quotes.
fn unquote(w: &[u8]) -> &[u8] {
    match (w.first(), w.last()) {
        (Some(b'"'), Some(b'"'))
        | (Some(b'\''), Some(b'\''))
        | (Some(b'`'), Some(b'`'))
        | (Some(b'['), Some(b']'))
            if w.len() >= 2 =>
        {
            w.get(1..w.len().saturating_sub(1)).unwrap_or(&[])
        }
        _ => w,
    }
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Tokens<'a> {
    fn at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i.saturating_add(k)).copied()
    }

    fn advance(&mut self, n: usize) {
        self.i = self.i.saturating_add(n).min(self.s.len());
    }

    /// Skip whitespace and comments. An unterminated block comment runs to
    /// the end, as in SQLite.
    fn skip_blank(&mut self) {
        loop {
            match (self.at(0), self.at(1)) {
                (Some(b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'), _) => self.advance(1),
                (Some(b'-'), Some(b'-')) => {
                    while let Some(c) = self.at(0) {
                        self.advance(1);
                        if c == b'\n' {
                            break;
                        }
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.advance(2);
                    loop {
                        match (self.at(0), self.at(1)) {
                            (None, _) => break,
                            (Some(b'*'), Some(b'/')) => {
                                self.advance(2);
                                break;
                            }
                            _ => self.advance(1),
                        }
                    }
                }
                _ => return,
            }
        }
    }

    /// The next token: a word, a quoted name or string, or one other byte.
    fn next(&mut self) -> Option<&'a [u8]> {
        self.skip_blank();
        let start = self.i;
        let first = self.at(0)?;
        let close = match first {
            b'"' => Some(b'"'),
            b'\'' => Some(b'\''),
            b'`' => Some(b'`'),
            b'[' => Some(b']'),
            _ => None,
        };
        if let Some(close) = close {
            self.advance(1);
            while let Some(c) = self.at(0) {
                self.advance(1);
                if c == close {
                    break;
                }
            }
        } else if is_word(first) {
            while self.at(0).is_some_and(is_word) {
                self.advance(1);
            }
        } else {
            self.advance(1);
        }
        self.s.get(start..self.i)
    }
}

/// Identifier characters: ASCII letters, digits, `_`, `$`, and every byte
/// of a multi-byte UTF-8 character.
fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_disguised_virtual_tables() {
        for yes in [
            "CREATE VIRTUAL TABLE x USING fts5(a)",
            "create virtual table x using fts5(a)",
            "CREATE/**/VIRTUAL TABLE v USING fts5(a)",
            "CREATE/* a comment */VIRTUAL/**/TABLE v USING fts5(a)",
            "CREATE -- a line comment\nVIRTUAL TABLE v USING rtree(id, a, b)",
            "CREATE\t\r\n\x0c VIRTUAL TABLE v USING fts5(a)",
            "CREATE \"VIRTUAL\" TABLE v USING fts5(a)",
            "  CREATE VIRTUAL TABLE x USING fts5(a)",
        ] {
            assert!(sql_declares_virtual_table(yes.as_bytes()), "{yes:?}");
        }
        let padded = format!("CREATE{}VIRTUAL TABLE v USING fts5(a)", " ".repeat(4096));
        assert!(sql_declares_virtual_table(padded.as_bytes()));
        let commented = format!(
            "CREATE/*{}*/VIRTUAL TABLE v USING fts5(a)",
            "x".repeat(4096)
        );
        assert!(sql_declares_virtual_table(commented.as_bytes()));
    }

    #[test]
    fn other_statements() {
        for no in [
            "",
            "CREATE",
            "CREATE TABLE virtual (a)",
            "CREATE TABLE x (virtual INTEGER)",
            "CREATE VIEW x AS SELECT 1",
            "CREATE VIRTUALTABLE",
            "CREATE INDEX virtual ON t(a)",
            "CREATE /* unterminated VIRTUAL TABLE",
            "-- CREATE VIRTUAL TABLE\nCREATE TABLE t (a)",
            "SELECT VIRTUAL",
        ] {
            assert!(!sql_declares_virtual_table(no.as_bytes()), "{no:?}");
        }
    }
}

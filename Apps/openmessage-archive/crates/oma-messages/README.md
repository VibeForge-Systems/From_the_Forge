# oma-messages

Reads the iPhone Messages database (sms.db) of an opened backup, for
OpenMessage Archive. It does not depend on Tauri, and it persists nothing.
The design is the Milestone 3 spec; the crate-level documentation in
`src/lib.rs` describes the flow.

## Message records

`SmsDb::for_each_message` reads each `message` row into a `MessageRecord`:
both body sources (the `text` column and the decoded `attributedBody`, with
neither preferred), the association (`assoc.rs`: tapbacks, stickers and poll
votes, with the `p:N/`, `bp:` and bare target forms), the reply facts, the
parsed `message_summary_info` (`summary.rs`: `otr`, `ec`, `rp` and `tmp`,
where a bad field spoils only itself) and a derived `EditLabel` (an ordered
list of rules with neutral labels). Coded values (`codes.rs`) are mapped
only where a source documents them, and the raw value is always kept.
`GuidSets` (`guidset.rs`) counts reply originators and association targets
that match no message, in Rust rather than SQL.

These modules were written from the facts recorded in the Milestone 3 spec
and `docs/supported-formats.md`; no GPL implementation was opened. The
summary is read through `oma_backup::bplist`, under its budgets.

Tests: `tests/messages.rs` (the pass on rows built in the test),
`tests/summary.rs` (plist shapes and bombs, through the test-only binary
plist writer in `tests/common/bplist.rs`), `tests/parser_mutation.rs`
(seeded mutations of summaries, association targets and every `message`
column through SQLite) and `tests/resume.rs`.

## Typedstream decoder (clean room)

`src/typedstream/` decodes the typedstream (`NSArchiver`) blobs in
`message.attributedBody`: [`decode_attributed_body`] returns the text, its
runs and parts, or one of a closed set of `TsErrorKind`s. It is budgeted
(`TsBudget`, whose limits a caller can only tighten) and fails closed: every
length is checked before it is used, every reference is bounds-checked, and
recursion is bounded by `max_depth`.

It is a clean-room implementation, written from format facts only:

- C. Sardegna's 2025 write-up of the format (prose facts, and the published
  sample as a hash anchor);
- the libmagic "Apple/NeXT typedstream" entry (BSD-2-Clause);
- Apple's type-encoding table and `NSString`/`NSArchiver` documentation;
- attribute key names, as facts, from imessage-database's documentation.

No GPL or LGPL implementation (crabstep, imessage-database,
imessage-exporter, pytypedstream, GCC libobjc, GNUstep) was opened while
writing it. pytypedstream was run only as a black-box oracle against the
synthetic vectors. The full record, file by file, is
[`scripts/fixtures/PROVENANCE.md`](../../scripts/fixtures/PROVENANCE.md);
the independent clean-room review is still pending.

Tests: `tests/typedstream_vectors.rs` (the committed vectors in
`tests/fixtures/typedstream` and generated hostile inputs),
`tests/typedstream_mutation.rs` (seeded mutations; `OMA_TS_MUTATIONS=<n>`
sets the iteration count, 20,000 by default) and
`tests/typedstream_roundtrip.rs` (through the test-only writer).

[`decode_attributed_body`]: src/typedstream/mod.rs

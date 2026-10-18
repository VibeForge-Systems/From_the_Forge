# Synthetic typedstream vectors

Test input for the typedstream decoder in `crates/oma-messages` (M3 spec
section 10). **Every value in these files is invented.** None was taken
from a real device or message. The mention handle `+15555550100` is a
fictional number (`SYNTH`).

Every file here is written by
[`scripts/fixtures/make_synthetic_typedstream.py`](../../../scripts/fixtures/make_synthetic_typedstream.py),
which is clean-room code (see
[`scripts/fixtures/PROVENANCE.md`](../../../scripts/fixtures/PROVENANCE.md)).
Do not edit them by hand.

## Layout

```text
<name>.bin      one typedstream blob
expected.json   for each vector: the decoder's expected public result
                ("public"), the generic decode checked by the crate's unit
                tests ("generic"), and the oracle check ("oracle")
SHA256SUMS      every file except this one and README.md
```

Vector families: `v*` attributed strings, `int_*` the 31 integer-head
probes (named `int_<type>_<hex>`; an upper-case type letter, `B`, `C`,
`I` or `Q`, is written with a `u` prefix, as in `int_uI_ff`, so that no two
names differ only by case), `ref_*` references (including indexes of 110 and above),
`nsdata_5c`, `array_5c`, `struct_nsrange`, and `h_*` hostile inputs. The
largest hostile inputs (16 MiB + 1, zero-width type elements, a type string
referenced a million times, 300,000 references to one object, half a million
runs sharing an 8 MiB dictionary) are generated inside
`crates/oma-messages/tests/typedstream_vectors.rs` instead of being
committed.

`.gitattributes` marks `tests/fixtures/**` as binary. The
`fixtures_integrity` test in `crates/oma-messages` checks every file
against `SHA256SUMS` and pins the SHA-256 of `SHA256SUMS` itself, so any
change shows up as a test failure that points at the oracle record.

## Trust anchors

1. **The published sample.** Before writing, the generator checks that its
   writer reproduces the 233-byte sample blob from C. Sardegna's write-up
   (SHA-256 `33162f000e9abad2ba56ebfcec6ef4ef671eb68048de1b0fc3c6bf2178412d9f`). Those bytes are not
   committed: the blob's licence is not stated.
2. **Oracle 2 (black box).** `scripts/fixtures/verify_typedstream_with_oracle.py`
   runs `pytypedstream` 0.1.0 (LGPL-3.0-or-later, pinned with its wheel
   hash in `scripts/fixtures/requirements-oracle-typedstream.txt`) as a
   subprocess. It never imports it. Each vector's `oracle.check` says what
   is compared:
   - `compare`: the oracle decodes the vector, and its structure (text,
     runs and dictionaries as written; scalars; strings) equals
     `oracle.view`;
   - `must_fail`: the oracle rejects the vector;
   - `exempt`: not checked, with the reason in `oracle.reason`. The `0x87`
     vectors are exempt because pytypedstream 0.1.0 rejects `0x87`, which
     Sardegna observed from Apple's encoder. Budget and policy vectors are
     exempt because they test our own limits.

   The run is recorded in `docs/compatibility.md`, keyed to the SHA-256 of
   `SHA256SUMS`.

Both anchors show only that our writer and our decoder agree with one
published blob and one independent decoder. Confirmation from Apple's own
encoder needs a Mac (a hardware question in `docs/supported-formats.md`).

## Regenerating

Linux (Python 3, standard library only; no SQLite involved, so the output
is byte-identical everywhere):

```sh
python3 -I scripts/fixtures/make_synthetic_typedstream.py tests/fixtures/typedstream
python3 -m venv .venv-oracle-ts
.venv-oracle-ts/bin/pip install --require-hashes -r scripts/fixtures/requirements-oracle-typedstream.txt
.venv-oracle-ts/bin/python -I scripts/fixtures/verify_typedstream_with_oracle.py tests/fixtures/typedstream
sha256sum tests/fixtures/typedstream/SHA256SUMS      # record with the oracle run
```

Windows is not a supported generation environment; the tests only read the
committed files.

| | |
|---|---|
| Generated with | Python 3.13.16 |
| Clean-room review | Pending |

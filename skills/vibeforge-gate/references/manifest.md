# Manifest and runner reference

`.vibeforge/gates.yaml` declares the checks. `.vibeforge/gate.sh` runs them.

## Shape

```yaml
meta:
  name: my-repo
  default_stage: push
  path_prepend: ~/.cargo/bin:./node_modules/.bin

checks:
  - id: unit-tests
    desc: the unit suite
    stage: commit
    run: npm test
```

## `meta`

| Key | Meaning |
|---|---|
| `name` | Shown in the header and summary. Defaults to the repo directory name. |
| `default_stage` | Stage for checks that do not declare one. Defaults to `push`. |
| `path_prepend` | Colon-separated directories prepended to `PATH` for the whole run. `~` expands. Use it for tools a non-login shell will not find: `~/.cargo/bin`, `./node_modules/.bin`, `./.venv/bin`, `~/go/bin`. Declaring this is what stops an installed tool from being reported missing. |

## `checks`

Each item needs at minimum `id` and `run`.

| Field | Meaning |
|---|---|
| `id` | Unique. Used by `--only`, `--explain`, and the summary. Duplicates are a manifest error. |
| `desc` | One line, shown in the header and `--list`. |
| `stage` | `commit`, `push`, or `manual`. See Stages. |
| `dir` | Working directory relative to the repo root. Default `.`. A missing directory is a blocking SKIP, not a pass. |
| `run` | Bash, run with `-euo pipefail` in `dir`. Single line or `\|` block. **Required.** |
| `needs` | Comma-separated binaries that must be on `PATH`. Any missing → blocking SKIP. |
| `probe` | Any command whose success means "ready". For things `command -v` cannot see: a library (`python3 -c 'import jsonschema'`), a running service, a mounted path. Failure → blocking SKIP. |
| `tool` | A single pinned binary, resolved before the check runs and exported as `$VF_TOOL`. |
| `tool_version` | The pin. **Enforced** — see Tool resolution. |
| `version_cmd` | How to ask the tool its version. Default `<tool> --version`. The pin must appear in the first 3 lines of output. |
| `install` | Printed when the check SKIPs. Make it a command the user can paste. |
| `fetch` | Bash that produces an executable at `$VF_TOOL_DEST`. Run only when the pinned tool is not already available. Also gets `$VF_TOOL_VERSION` and `$VF_CACHE_DIR`. |
| `env` | `KEY=VALUE` per line, passed to `run`. Values are literal — no shell expansion. For a path built from `$VF_ROOT`, set it inside `run` instead. |
| `skip_env` | Name of an env var that, set to `1`, deliberately skips this check. Reported, **does not block**. |
| `enforce` | `false` makes the check advisory: it runs, a failure reports `WARN`, and it never blocks. Shadow mode. Default `true`. |
| `why` | Rationale, shown by `--explain`. Say why the check earns its runtime. |
| `mirrors` | Provenance — where this check came from. Useful after importing an existing CI config. |

### Variables available to `run`

| Variable | Value |
|---|---|
| `$VF_TOOL` | The resolved pinned binary (empty if the check declares no `tool`) |
| `$VF_ROOT` | Absolute path to the repo root |
| `$VF_CACHE_DIR` | Where pinned tools are cached |

Use `$VF_TOOL` rather than the bare tool name — that is what guarantees the check
runs the pinned build and not whatever is on `PATH`.

## Stages

| Stage | Meaning |
|---|---|
| `commit` | Fast, always-on. Runs at pre-commit and at every later stage. |
| `push` | The full bar. Includes all `commit` checks. |
| `manual` | Excluded from the default run. Reached with `--all` or `--only`. |

`--stage push` runs `commit` + `push`. There is exactly one full bar, so the
pre-commit subset cannot drift away from it.

## Tool resolution

For a check with `tool` and `tool_version`, in order:

1. **Cached pinned build** at `$VF_CACHE_DIR/<tool>-<version>` — used directly.
2. **A copy on `PATH`** — used *only if* `version_cmd` output contains the pin.
3. **`fetch:`** — run once, result cached, then used.
4. **Blocking SKIP**, naming what was found and printing `install`.

A tool present at the wrong version is a SKIP, not a pass. This is deliberate:
a gate running a different scanner than the one you pinned is not the gate you
reviewed. Pre-warm every pinned tool with `gate.sh --fetch`.

With `tool` but no `tool_version`, any copy on `PATH` is accepted.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Every selected check ran and passed |
| 1 | At least one check FAILED |
| 2 | At least one check was SKIPPED — a coverage gap. `VF_GATE_ALLOW_SKIP=1` accepts it and exits 0 |
| 3 | Usage error, or a manifest the parser cannot represent |

Deliberate skips (`skip_env`) and advisory failures (`enforce: false`) do not
affect the exit code.

## Command line

```
gate.sh                     # full bar (stage: push)
gate.sh --stage commit      # fast subset
gate.sh --all               # everything, including stage: manual
gate.sh --only a,b          # just these ids, regardless of stage
gate.sh --list              # declared checks, stages, skip switches
gate.sh --explain [id]      # rationale, provenance, and the exact command
gate.sh --fetch             # pre-download pinned tools into the cache
```

| Environment | Effect |
|---|---|
| `VF_GATE_ALLOW_SKIP=1` | Exit 0 despite blocking SKIPs |
| `VF_GATE_MANIFEST` | Use a different manifest |
| `VF_GATE_CACHE` | Where pinned tools are cached (default `~/.cache/vibeforge-gate`) |
| `VF_GATE_NO_COLOR=1` | Plain output. `NO_COLOR` is honored too |

## The parser's accepted subset

The runner has no YAML dependency. It accepts exactly this, and **fails with a
line number on anything else** — it will never silently misread a manifest:

- `meta:` and `checks:` at column 0.
- `meta` entries at indent 2, as `key: value`.
- Checks: `  - key: value` at indent 2 opens an item; `    key: value` at
  indent 4 adds to it.
- Block scalars: `key: |` or `key: |-`, with the following more-indented lines
  taken verbatim and the common indent stripped.
- Comments only when `#` is the first non-space character on the line. **Inline
  comments are not stripped**, so a command containing `#` survives intact.
- Values may be bare, `'single-quoted'` or `"double-quoted"`; outer quotes are
  stripped with no escape processing.

Not supported: nested maps, inline `[a, b]` or `{k: v}`, anchors, aliases,
multi-document files, `>` folded scalars, tabs for indentation. If you need one
of those, you are probably trying to put logic in the manifest that belongs in a
script the manifest calls.

## Worked example

```yaml
  - id: vuln-scan
    desc: no known-vulnerable dependencies
    stage: push
    tool: osv-scanner
    tool_version: "1.9.2"
    install: "go install github.com/google/osv-scanner/cmd/osv-scanner@v1.9.2"
    why: |
      The dependency-review equivalent. Runs against the lockfile, so it sees
      transitive pins rather than declared ranges.
    run: |
      "$VF_TOOL" --lockfile=go.mod --format table
```

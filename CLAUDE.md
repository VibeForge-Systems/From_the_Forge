# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

`vibeforge-gate` ships a **repository-owned merge bar**: `.vibeforge/gates.yaml`
declares checks, `.vibeforge/gate.sh` runs them, git hooks refuse work that does
not pass. It is packaged two ways at once — as a Claude Code plugin
(`.claude-plugin/plugin.json` + `commands/gate.md` → `/vibeforge-gate:gate`) and as a plain skill
(`skills/vibeforge-gate/`). The skill directory is self-contained; nothing in it
depends on the plugin wrapper.

There is no build step and no application code. The deliverables are a bash
script, a set of YAML manifests, and the prose that tells an agent how to use
them — so "the docs" are product, not commentary.

## Commands

Everything runs under bash (4+), coreutils and git. On Windows use the Bash tool,
not PowerShell.

```sh
.vibeforge/gate.sh                   # the full bar (stage: push) — run before any PR
.vibeforge/gate.sh --stage commit    # the fast subset
.vibeforge/gate.sh --only shellcheck # one check, regardless of stage
.vibeforge/gate.sh --list            # inventory: ids, stages, skip switches
.vibeforge/gate.sh --explain dco     # a check's rationale, provenance, exact command
.vibeforge/gate.sh --fetch           # pre-download pinned tools into the cache

./selftest.sh                        # 21 cases pinning the runner's contract
./selftest.sh -v                     # ...with the runner output for each case
```

The selftest runs `skills/vibeforge-gate/templates/gate.sh` (the template, not the
dogfooded copy) against throwaway repos in `$TMPDIR`. There is no per-case filter;
run the whole file.

Exit codes are load-bearing and mean the same thing everywhere in this project:
**0** all selected checks ran and passed · **1** a check FAILED · **2** a check
SKIPPED (coverage gap — `VF_GATE_ALLOW_SKIP=1` accepts it) · **3** manifest or
usage error.

## The contract

> Green means every declared check actually ran and passed.

Four rules protect that sentence, and they govern most judgment calls in this
codebase (full reasoning in `skills/vibeforge-gate/references/doctrine.md`):

1. A missing tool — or a tool at the wrong pinned version — is a `SKIP`, and a
   SKIP exits 2. Never "fix" a SKIP by deleting the check, adding `|| true`, or
   dropping it to `enforce: false`; that converts a visible gap into an invisible
   one.
2. Tool versions are pinned and the pin is enforced at resolution time.
3. `skip_env:` (operator chose to skip) is reported differently from a coverage
   gap and does not block. Never route a missing tool through that path.
4. Claim only what you check. Narrowing a coverage claim is always welcome;
   widening one needs the check to exist first.

When reporting a gate run to the user, report SKIPs as SKIPs. A run with SKIPs is
not green.

## Architecture

**Two layers.** The *skill* (`skills/vibeforge-gate/SKILL.md` + `references/` +
`catalog/`) is doctrine and copy-ready checks that teach an agent to build a gate.
The *machinery* (`templates/gate.sh`, `templates/hooks/*`, `templates/gates.yaml`)
is what gets copied into a target repo's `.vibeforge/`. Changes to how a gate
behaves go in the machinery; changes to how an agent reasons about gates go in the
skill prose.

**The runner is one file, ~480 lines, no dependencies.**
`skills/vibeforge-gate/templates/gate.sh` is: a hand-written parser for a strict
YAML subset (fields land in the associative array `F["<index>.<field>"]`, meta in
`META[]`), `resolve_tool` (cached pinned build → `PATH` copy whose `version_cmd`
output contains the pin → `fetch:` → blocking SKIP), `run_check` (skip_env →
`needs` → `probe` → tool resolution → `dir` exists → `bash -euo pipefail -c` the
`run:` block), then verdict recording and the exit-code decision. It carries no
repo-specific content, which is why updating an installed gate is a `cp`.

**The parser rejects rather than guesses.** No nested maps, no inline flow
collections, no anchors, no folded scalars, no tabs; anything outside the subset
dies with a line number. Inline `#` is *not* stripped, so commands containing `#`
survive. Reference: `references/manifest.md`. If a manifest wants a construct the
parser lacks, the logic probably belongs in a script the manifest calls.

**Stages are a superset chain.** `commit` ⊂ `push`; `manual` is excluded from the
default bar. There is exactly one full bar, so the pre-commit subset cannot drift
away from it.

**This repo gates itself,** so `gate.sh` and the hooks exist twice:

| Path | Role |
|---|---|
| `skills/vibeforge-gate/templates/gate.sh`, `templates/hooks/*` | source of truth — what ships |
| `.vibeforge/gate.sh`, `.vibeforge/hooks/*` | the copy this repo runs on itself |

Edit the template, then `cp skills/vibeforge-gate/templates/gate.sh .vibeforge/gate.sh`
(and the same for each hook). The `templates-in-sync` check fails on drift.

**Catalog and example manifests are parsed by the real parser** in the
`catalog-parses` check, so a block that the runner would reject cannot merge.

**`.vibeforge/gates.yaml` is also documentation** — its header names what the gate
does *not* cover, and every check carries a `why:` surfaced by `--explain`.

## Conventions

- **Sign off every commit** (`git commit -s`). The `dco` check verifies every
  non-merge, non-`*[bot]` commit ahead of `main`. Retrofit with
  `git rebase --signoff <base>`.
- Topic branch off `main`; never commit to `main` directly.
- **Changing runner behavior requires a `selftest.sh` case** for the behavior
  changed. The exit-code/SKIP/pin/parser semantics are the product, so they are
  tested rather than documented and hoped for.
- **Stay dependency-light**: bash 4+, coreutils, git. No YAML library, language
  runtime, or package manager. A new runtime dependency breaks the portability
  claim the package rests on.
- New catalog checks need a pinned `tool_version:` (when the tool sits outside the
  project's dependency graph), a pasteable `install:`, and a `why:`.
- A check that would fail on today's codebase goes in with `enforce: false`
  (shadow mode) and a deadline, then is promoted by deleting that line.
- `skills/vibeforge-gate/examples/*/reference-*` are verbatim copies of a
  predecessor gate, kept as evidence for the equivalence audit in
  `EQUIVALENCE.md`. Do not edit or lint them — `shellcheck` deliberately excludes
  them.
- `skills/vibeforge-gate/evals/` holds skill-trigger and behavior evals; update
  them when the skill's `description` frontmatter or routing changes.

## Honest limits to restate, not round up

A local hook is bypassable (`--no-verify`) and per-clone opt-in, so it is a
convenience, not an enforced boundary. The gate reproduces *commands*, not
environments — one machine, one OS, one architecture. A green gate means the
declared checks passed, not that the manifest declares the right ones. Say this
plainly whenever installing or describing a gate.

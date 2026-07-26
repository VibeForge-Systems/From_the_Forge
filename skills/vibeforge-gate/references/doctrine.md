# Doctrine

Read this before making a judgment call about a gate. The mechanics are in
`manifest.md`; this is the reasoning the mechanics encode.

## Why a repository should own its merge bar

A hosted CI service is a rental. The gates stop when the rental does — when the
minutes run out, when the account lapses, when an org migrates forges, when the
free tier changes shape, when the network is down, when you are on a plane. The
checks were catching real defects right up until the moment they stopped, and
nothing about the code got safer at that moment.

There is a second, quieter version of the same problem: the check you want
exists, but sits behind a tier you are not paying for. Secret scanning, static
analysis, dependency review — these are frequently sold as add-ons. The
underlying tools are open source and run fine on a laptop. What you are renting
is the integration, not the capability.

A repository-owned gate answers both. The checks are declared in the repository,
run from the repository, and versioned with the repository. They work offline,
cost nothing per run, and cannot be turned off by a billing event. When you do
have somewhere to run them centrally, you still can — the manifest is the source
of truth, and a hosted runner becomes one more consumer of it rather than its
owner.

**This is a real trade, not a free win.** See "What you give up" at the end.

## The contract, and the four rules that keep it true

> **Green means every declared check actually ran and passed.**

That sentence is the entire product. Everything below protects it.

### 1. A missing tool is a SKIP, and a SKIP blocks

The most dangerous gate is not one that fails. It is one that goes green because
a check silently did not execute — the scanner was not installed, the optional
dependency was absent, the tool moved. Every one of those is a *coverage gap*
wearing a passing badge.

So: a check whose tool cannot be found is reported `SKIP`, loudly, by name, with
the command that would install it — and the run exits **2**. Not 0.

The gap can be accepted. `VF_GATE_ALLOW_SKIP=1` exits 0 despite skips. That is
the right escape hatch precisely because someone had to type it, and it shows up
in shell history and CI logs. What is not available is skipping *by accident*.

**Corollary:** never "fix" a SKIP by deleting the check, wrapping it in
`|| true`, or lowering it to `enforce: false`. Those turn a visible gap into an
invisible one, which is strictly worse than the SKIP you started with.

### 2. Pin tool versions, and enforce the pin

An unpinned scanner is a build that can fail with no diff — someone else's
ruleset update lands in your merge. It is also a check that can quietly get
*weaker*, which you will not notice at all.

So every tool gets `tool_version:`, and the runner enforces it: a copy on `PATH`
is used only if it reports the pinned version. Otherwise the runner reaches for
the cached pinned build, then `fetch:`, then SKIPs. **A tool at the wrong
version is treated as missing.** A gate running a different scanner than the one
you reviewed is not the gate you reviewed.

Where the pin comes from, per ecosystem, is in `adapting-stacks.md`. Bumping a
pin should be a deliberate one-line commit that a reviewer can see.

### 3. A deliberate skip is not a coverage gap

Some checks are legitimately optional at some moments — a five-minute race
detector pass during a docs-only fix, an integration suite needing a service
that is not up. Declaring those `skip_env:` gives the operator a named switch:
the check is skipped, reported on screen as deliberate, and does **not** block.

The distinction is the whole point. "I chose to skip this and can see that I
did" and "this never ran and nobody knows" both leave the check unrun, but only
the first is a decision. Keep them visibly different in the output, and never
reuse the deliberate path to paper over a missing tool.

### 4. Claim only what you check

A gate's documentation is a safety claim, and people act on it. Write down what
is *not* covered as carefully as what is:

- The gate runs on one machine — yours. Not a matrix.
- It reproduces commands, not environments.
- A local hook is bypassable by anyone who types `--no-verify`.
- A green run means the *declared* checks passed, not that the declaration is
  complete.

The instinct to round "mostly reproduces CI" up to "reproduces CI" is exactly
the instinct to resist. Overstatement gets discovered at the worst moment, and
it costs the gate its credibility permanently — after which people stop reading
the output at all.

## Shadow mode

A check introduced in blocking mode on day one, that fails on existing code, is
a check someone will disable — usually along with its neighbours.

Introduce it with `enforce: false`. It runs, reports `WARN` on failure, and does
not block. Tune it against real runs until the noise is gone, fix or accept what
it found, then promote it by deleting the line. Same check, same command; the
only change is whether it can stop a push.

Shadow mode is honest exactly as long as everyone knows a check is in it. `--list`
marks advisory checks; leaving one there for a year is how a gate quietly
becomes decorative. Give it a deadline.

## Staging

- `stage: commit` — fast, always-on. Format, lint, structural invariants, fast
  unit tests. If it takes longer than a few seconds, it does not belong here;
  a slow pre-commit hook is a hook people uninstall.
- `stage: push` — the full bar. Includes every commit check.
- `stage: manual` — slow, occasional, or environment-dependent. Excluded from
  the default bar; runs with `--all` or `--only`.

Because push includes commit, there is exactly one full bar and no way for the
two to drift apart.

## What belongs in a gate

**Yes:** deterministic checks that fail for a reason in the diff. Build, tests,
format, lint, type checks, structural/architectural invariants, secret scanning,
dependency vulnerabilities, license policy, schema and fixture validation,
commit-metadata rules like sign-off.

**No:** anything flaky (a check that fails randomly trains people to re-run
until green, which destroys every other check's authority); anything needing
production credentials; anything whose failure is not actionable by the person
pushing; anything that takes longer than the patience of the person waiting.

A slow-but-valuable check is not excluded — it is `stage: manual`, or
`skip_env:`-able, or both.

## What you give up

State these when you install a gate:

- **Enforcement.** A local hook is a convenience. `--no-verify` bypasses it, and
  a contributor who never runs `core.hooksPath` is never gated at all. Only a
  check that runs on the receiving end — a self-hosted forge, a shared runner, a
  `post-receive` hook — is unbypassable. If you have one, put the same manifest
  behind it; that is the whole point of the manifest being the source of truth.
- **Environment coverage.** One machine, one OS, one architecture, whatever
  toolchain you happen to have. Cross-platform matrices are the one thing hosted
  CI genuinely gives you that a laptop cannot.
- **Independence.** CI runs on a clean checkout. Your working tree has untracked
  files, local config, and a warm cache. A check can pass locally for reasons
  that will not exist elsewhere — scope scans to tracked files where it matters,
  and know that this gap exists.

None of these is a reason not to run the gate. They are the reasons not to
oversell it.

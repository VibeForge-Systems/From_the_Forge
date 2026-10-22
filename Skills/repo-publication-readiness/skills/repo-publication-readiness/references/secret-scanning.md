# Secret scanning before publication

How `scripts/scan-secrets.sh` uses gitleaks and TruffleHog, what each misses on its own, and how to triage what they find. Tool behavior below was read from each tool's source at the tag named; see `verification-log.md`.

## Contents

- Why two scanners
- gitleaks (v8.30.1)
- TruffleHog (v3.99.3)
- The blind spots both share
- Scope: which refs
- Triage
- Rotation

## Why two scanners

They fail differently. gitleaks is a fast, offline pattern-and-entropy engine with a large, reviewable ruleset. TruffleHog has fewer rules for some formats but **verifies** candidates against the issuing provider, which turns "this looks like a key" into "this key is live". A pre-publication audit wants both: gitleaks for breadth, TruffleHog to rank what must be rotated first.

## gitleaks (v8.30.1)

- **Subcommands.** `git`, `dir` (aliases `directory`, `file`) and `stdin`. `detect` and `protect` still run but are hidden and deprecated since 8.19. The mapping, from gitleaks' own source: `detect --source R` → `git R`; `protect --staged` → `git --pre-commit --staged`; `detect --no-git` → `dir`; `detect --pipe` → `stdin`.
- **Default history scope.** With no `--log-opts`, `gitleaks git` runs `git log -p -U0 --full-history --all --diff-filter=tuxdb` — every ref already. **Passing `--log-opts` replaces that argument list; it does not add to it.** `--log-opts="--all"` therefore *drops* `--full-history` and the diff filter. For a full audit pass no `--log-opts`. For a range, pass exactly the range.
- **`--log-opts` is split on single spaces** and handed to `git log` as separate arguments; quoted values inside it do not survive (gitleaks warns about this). Build long SHA lists as one space-joined string, and chunk them.
- **Exit codes.** `--exit-code N` sets the code for "leaks found" (default 1). `scan-secrets.sh` uses 99, so a tool error can never read as clean.
- **Config.** In order: `--config`, `$GITLEAKS_CONFIG`, `$GITLEAKS_CONFIG_TOML`, then `<target>/.gitleaks.toml`. A `.gitleaksignore` holds fingerprints (`commit:file:rule:line`) of reviewed findings.
- **Maintenance.** 8.30.1 is the newest release as of October 2026; the project has moved to security-patch-only maintenance and its author's successor, Betterleaks (v1.9.0 at that date), is a separate tool. Pin 8.30.1 and re-evaluate rather than tracking `latest`.

## TruffleHog (v3.99.3, AGPL-3.0)

- **`file://` clones by default.** `trufflehog git file://PATH` clones the repository into a temporary directory before scanning, and a clone carries only branches and tags — not refs/pull/*, notes, stash or remote-tracking refs. Tested: a token present only in a mirror's `refs/pull/1/head` gave no results by default and one result with `--bare --trust-local-git-config`. `--trust-local-git-config` scans the repository in place instead. That flag trusts the repository's git config (which can name programs git will run), so use it only on a clone you made yourself — `scan-secrets.sh` uses it for bare mirrors and when given `--trust`.
- **`--results`.** Any of `verified`, `unknown` (verification errored), `unverified`, `filtered_unverified`. The default is `verified,unverified,unknown`. `--only-verified` still exists but is a hidden flag; use `--results`. For an audit, keep the default: an *unverified* key may belong to a provider TruffleHog cannot check.
- **`--fail`** exits 183 when results are found. Without it TruffleHog exits 0 regardless.
- **Verification is a network call** to the issuer with the candidate value. `--no-verification` keeps candidates local.
- **Beyond git.** `trufflehog github --repo=URL --issue-comments --pr-comments --include-wikis` scans issue and PR text and the wiki — the surfaces a flip publishes that a git scan never sees. Needs a token with read access.
- **Ranges.** `--since-commit BASE --branch HEAD` limits a git scan.

## The blind spots both share

Demonstrated here, not inferred:

1. **Merge resolutions.** Both scanners build on `git log -p` (TruffleHog: `git log --patch --full-history`), and `git log -p` prints no diff for a merge commit. A value introduced while resolving a conflict is invisible to both history modes. With a token planted only in a merge resolution, `gitleaks git` (8.30.1) reported no leaks and `trufflehog git` (3.99.3) returned nothing; replaying the merge with `git show --remerge-diff` into either scanner found it. `--remerge-diff` (git ≥ 2.36) shows exactly what the resolution added relative to an automatic re-merge; `--cc` is the older, coarser fallback.
2. **Commit and tag messages** are not diffs. A token pasted into a commit message (it happens, usually in a "rotated X" message) is in every clone. `scan-secrets.sh` feeds all messages to `gitleaks stdin`.
3. **Binary content.** Line-diff scanners do not read a SQLite file, an office document or an image. `sweep-leaks.py` sweeps binary blobs as extracted strings for the non-credential leak classes; a credential inside a binary needs a format-aware tool.
4. **Unreachable objects.** Not in any `git log`. They are also not sent by a branch or tag push, so for the fresh-repo path they do not matter; on a server copy (the flip path) they are GitHub's to purge.

## Scope: which refs

Always scan a **mirror** (`git clone --mirror`). On GitHub a mirror includes `refs/pull/*/head` and `refs/pull/*/merge` for every pull request ever opened — public after a flip, and read-only to you, so a rewrite cannot reach them. A normal clone has none of them, nor notes or stash.

## Triage

Every finding gets one disposition (the table in SKILL.md), recorded with a reason. Practical rules:

- **Real until proven otherwise.** "It's a test key" is a claim; check it with the issuer or rotate it anyway.
- **Suppress narrowly.** A `.gitleaksignore` fingerprint names one finding in one commit; a rule allowlist in `.gitleaks.toml` can hide a whole class. Prefer the fingerprint.
- **Synthetic fixtures.** Generate credential-shaped test values at run time rather than committing them; this skill's own selftest does that, which is why it can ship in a public repository without tripping the scanners or GitHub push protection.
- **Partner patterns.** On a public repository GitHub's secret scanning for partner patterns runs regardless of settings and notifies the issuer directly. A committed live AWS, GitHub or Slack credential will likely be revoked by its issuer — after it was public.

## Rotation

Rotate every credential that was ever committed, before any rewrite. Then:

1. Read the issuer's audit log for use between the commit date and now.
2. Confirm nothing still verifies: `trufflehog git file:///work/REPO.git --trust-local-git-config --results=verified`.
3. Record what was rotated, when, and by whom. The record is what lets phase 3's gate be checked by someone else.

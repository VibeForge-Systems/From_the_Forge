# Verification log

How each version- or platform-dependent claim in this skill was checked, and when. Tools and platforms change; re-verify a claim before relying on it, and update this log when you do. Last full pass: **2026-10-22**.

Method key:

- **source** — read in the tool's own source code at the stated tag
- **ran** — executed in a test environment and the result observed
- **spec** — read in GitHub's OpenAPI description (github/rest-api-description, commit of 2026-10-18)
- **docs** — read on docs.github.com or the github/docs source (main, 2026-10-21)
- **primary** — the publisher's own page or statute site
- **secondary** — a reputable copy or summary, because the primary would not load; named

## Tools

| Claim | Method | Notes |
|---|---|---|
| gitleaks newest release is 8.30.1 | source (git tags); release binary downloaded and its sha256 matched vibeforge-gate's pin (`88f91962…`) | |
| `detect`/`protect` hidden and deprecated; `git`/`dir`/`stdin` replacements | source, v8.30.1 `cmd/detect.go`, `cmd/protect.go` | |
| No `--log-opts` → `git log -p -U0 --full-history --all --diff-filter=tuxdb`; `--log-opts` replaces it | source, v8.30.1 `sources/git.go` | Corrects advice to pass `--log-opts="--all"`, which drops `--full-history` |
| `--log-opts` split on single spaces | source, v8.30.1 `sources/git.go` | |
| Config precedence; `--exit-code`; `--redact` default 100% | source, v8.30.1 `cmd/root.go` | |
| Betterleaks exists (v1.9.0) | source (git tags) | gitleaks' maintenance status is from its project notes; not re-read here |
| TruffleHog newest release is v3.99.3 | source (git tags); release binary downloaded, sha256 matched the release's checksums file (`029b182e…`) | Same-origin checksum: proves integrity, not authorship (the release is also cosign-signed; not checked) |
| `--only-verified` is hidden; `--results` values and default | source, v3.99.3 `main.go` | Resolves an open question from the original research |
| `--fail` exits 183 | source, v3.99.3 `main.go` | |
| `git file://` clones first unless `--trust-local-git-config` | source, v3.99.3 `pkg/sources/git/git.go` | Corrects advice to scan a mirror with plain `file://` |
| Both scanners miss merge-resolution content | source (both build on `git log -p`/`--patch` with no merge diff) + ran: with a token only in a merge resolution, `gitleaks git` 8.30.1 reported no leaks and TruffleHog 3.99.3 `git` returned no results; the `--remerge-diff` replay was caught by both | |
| `trufflehog git file://MIRROR` misses refs/pull/*; `--bare --trust-local-git-config` scans in place | source + ran (3.99.3: token only in `refs/pull/1/head` — 0 results by default, 1 in place; gitleaks 8.30.1 found it either way) | |
| git-filter-repo 2.47.0 has `--sensitive-data-removal`/`--sdr` and `--no-fetch`; needs git ≥ 2.22 | source + ran (2.47.0 from PyPI) | |
| `--replace-message` cleans tag messages | ran (selftest) | |
| `regex:` replacements are Python `re` templates (`\U` fails) | ran | Found while building the selftest |
| BFG v1.15.0 is the newest release | source (git tags) | Release date and Java 11 requirement from its releases page, not re-read |
| reuse 6.2.0; REUSE spec 3.3; REUSE.toml supersedes dep5 (deprecated) | primary (PyPI, fsfe/reuse-website, reuse-tool changelog) | |
| grant v0.6.8; deny-all default; `OR` treated as `AND` up to 0.6.8 | source (anchore/grant main and tags) | |
| zizmor 1.31.1; non-zero exit on findings; `--offline`, `--min-severity` | ran (PyPI build) | |
| Scorecard newest release v5.6.0 | source (git tags) | Its CLI flags were not re-run here |
| actions/checkout v4 → v4.4.0 → `11d5960a…`; softprops/action-gh-release v2 → v2.6.2 → `3bb12739…` | source (git ls-remote) | Used to pin From_the_Forge's release workflow |

`scan-secrets.sh`, the selftest's scanner cases, and vibeforge-gate's gitleaks-based catalog blocks were run end to end with the gitleaks 8.30.1 and TruffleHog 3.99.3 release binaries. TruffleHog verification was enabled for the From_the_Forge audit; whether provider APIs were reachable from the build environment was not established, so "no verified results" there means no results at all, verified or not.

## GitHub

| Claim | Method |
|---|---|
| Consequences list for private → public (rulesets disabled, Actions logs public, stars erased, private forks detached, GHAS access) | docs, "Setting repository visibility" (source 2026-08-14) |
| Private-fork commits join the public network on a flip | docs, forks reference |
| No documented auto-enable of secret scanning or repo push protection on a flip; push protection off by default at repo level; user push protection on by default | docs (secret scanning, push protection; 2026-07 to 2026-10) and changelog 2024-03-11 |
| Partner patterns always on for public repos | docs, "Secret scanning for partners" |
| Packages: container/npm/NuGet/RubyGems keep own visibility; Maven/Gradle follow the repo; public is irreversible | docs, Packages access control (2026-09-11) |
| Pages from a private repo on Free/Pro is public | docs, creating a Pages site |
| Wiki and Discussions public on a public repo; releases readable by anyone with read access | docs |
| Email push block checks only the most recent commit | docs (2026-03-12) |
| Noreply format and the 18 July 2017 cutoff | docs, email addresses reference |
| Private vulnerability reporting path; not on by default | docs (2026-10-02) |
| Fork-PR approval options and default; 30-day pending runs expire (not deleted) on GitHub.com | docs (2026-09-29) — corrects "auto-deleted after 30 days" |
| Code scanning never on by itself | docs |
| Sensitive-data removal procedure and Support conditions | docs (2026-08-14) |
| REST fields: `sha_pinning_required`, `default_workflow_permissions`, `can_approve_pull_request_reviews`, fork-PR `approval_policy` values, PVR `enabled`, vulnerability-alerts 204/404, automated-security-fixes, code-scanning default-setup `state`, `security_and_analysis` keys | spec |
| `rules/branches/{branch}` returns ruleset rules only (classic protection read separately) | spec + ran against a real repository |

## Exposure and exploitation

| Claim | Method |
|---|---|
| Truffle Security CFOR findings (July 2024) and GitHub's "by design" response | primary (Truffle Security), secondary (The Register), docs (forks) |
| Orca ~2 min; Unit 42 within 5 min; Clutch 1 min fastest, 6.6 min average | primary, each study |
| GH Archive: events since Feb 2011; email local parts hashed, domains kept, since May 2016; no removal process | primary (gharchive.org source, crawler code) |
| PushEvent payloads lost the commit list in October 2025 | primary (GitHub changelog 2025-08-08) + docs (event types) |
| Software Heritage coverage, revisit cadence, takedown process | primary (swh-docs sources); the website itself could not be loaded |

The original research cited a Comparitech "1 minute" figure; it was not traced to a primary source and is not used.

## Law and practice

| Claim | Method |
|---|---|
| California Lab. Code §§2870–2872 text and history | secondary (verbatim copy in github/balanced-employee-ip-agreement; FindLaw current to 2026-01-01); leginfo.legislature.ca.gov could not be loaded |
| Florida has no invention-assignment statute; hired-to-invent common law; §542.335 and the 2025 CHOICE Act do not address IP | primary (flsenate.gov for §542.335), secondary (two 2026 practice guides; case law via FindLaw) — a negative finding |
| New York §203-f (S5640, signed 2023-09-15, effective immediately) | primary (nysenate.gov) |
| The state list and notice requirements | secondary (github/balanced-employee-ip-agreement and law-firm summaries); NC and NJ statutes primary |
| North Dakota as an employer-default state | secondary, single source — **unverified** |
| Release-process practices (Google, Microsoft, TODO Group, OpenSSF Concise Guide) | primary: TODO Group and OpenSSF source repositories (2026-10-22); Google's open source release docs (read during the original research, not re-read); Microsoft's releasing page could not be loaded — its program summary page was used |

## Still open

- Whether opting a repository out of the GitHub Archive Program also stops Software Heritage's own crawl.
- What happens to an Enterprise Cloud privately published Pages site when its repository becomes public.
- The North Dakota statute.

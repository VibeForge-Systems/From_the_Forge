---
name: repo-publication-readiness
description: Audits a private GitHub repository for everything that would leak if it were made public, then guides the cleanup and the release itself. Covers credentials across every ref (two scanners plus the merge-resolution, commit-message and binary-content gaps both miss), commit identities, internal hostnames and IPs, employer and client names, credential rotation, history rewriting with git-filter-repo, issues/PRs/Actions logs/wiki and other non-code surfaces, licensing and provenance, employer IP clearance, Actions and branch hardening, and post-publish verification. Use when making a private repo public, open-sourcing a project, preparing a first public release, deciding between flipping visibility and publishing a fresh repo, or responding to something sensitive found in a repo that is about to go (or just went) public. Not for standing pre-push gates on an already-public repo — that is vibeforge-gate.
compatibility: Needs git 2.22+ (2.36+ for exact merge-resolution scanning), bash 4+, python3 3.11+, git-filter-repo 2.47+ for rewrites, gitleaks 8.19+ and trufflehog v3 for credential scans, and an authenticated gh for GitHub settings. Scripts never change GitHub settings and never push.
metadata:
  owner: VibeForge-Systems/From_the_Forge
  surface: repo-publication-readiness
  version: "1"
---

# Repository publication readiness

Making a repository public is the one git operation with no undo. Everything in it — every commit on every ref, every author address, every issue comment and Actions log — becomes readable, forkable and archived by third parties within minutes. This skill exists to make that a decision taken on evidence rather than a toggle flipped on hope.

## The contract

**Nothing is published until every phase gate below has passed, and every finding has exactly one recorded disposition.** Four rules keep that true:

1. **Rotate before you rewrite.** A credential that was ever committed is compromised. Rewriting history hides it from future clones; only revoking it makes it dead. Leaked cloud keys are exploited in minutes (`references/github-exposure.md`), so rotation is never deferred to "after cleanup".
2. **Publish a fresh repository, not the old one.** Flipping visibility publishes the entire network: refs/pull/* of every PR (read-only to you, so a rewrite cannot reach them), commits from private forks, and the full Actions log history. A new repository that receives only `refs/heads/*` and `refs/tags/*` from a verified clone publishes exactly what you checked. Flip only when issue/PR history is essential, and then audit those surfaces item by item.
3. **A scan that did not run is not a clean scan.** Every script here exits non-zero when a tool is missing or a pass did not complete, and says so. Report an incomplete result as incomplete.
4. **Keep the evidence out of the evidence.** Reports, deny lists and identity lists live outside the repository. The scripts refuse paths inside it, because a report committed by accident publishes every finding at once.

## Decide the path first

| If… | Then |
|---|---|
| History has no value worth keeping, or is badly contaminated | **Fresh root**: one orphan commit of the cleaned tree. Simplest to verify; loses blame, bisect and attribution (record contributors in a CONTRIBUTORS or NOTICE file). |
| History is worth keeping | **Filtered history** in a fresh repository: rewrite with git-filter-repo, verify, push heads and tags only. The default. |
| Issues and PR discussions must stay attached | **Flip in place** — the riskiest path. Requires the full non-code audit (phase 5) and accepts that refs/pull/* and old Actions logs go public as they are. Push rulesets are disabled by the flip; re-check them after. |

## Phases

Copy this checklist into the task list and tick items off. Each phase ends at a gate; do not start the next until its gate holds.

```
[ ] 0 Authority   written clearance to publish; repository frozen
[ ] 1 Inventory   mirror clone; inventory.sh; GitHub surfaces listed
[ ] 2 Scan        scan-secrets.sh + sweep-leaks.py on the mirror; every finding triaged
[ ] 3 Rotate      every credential ever committed revoked; none still verifies
[ ] 4 Rewrite     filter-repo (or fresh root); verify-rewrite.sh passes
[ ] 5 Surfaces    issues, PRs, releases, wiki, Actions, packages decided
[ ] 6 License     LICENSE (+NOTICE), REUSE lint, vendored code cleared, DCO/CLA chosen
[ ] 7 Harden      community files, workflow hardening, the gate installed
[ ] 8 Publish     new repo created + protected BEFORE the first push; heads/tags pushed
[ ] 9 Verify      check-github-settings.sh; public clone re-scanned
```

### 0. Authority

Confirm, in writing, that the code is yours (or your organization's) to release. For anyone with a day job this is the gate most often skipped and the one that cannot be fixed afterward: invention-assignment clauses can reach side projects that relate to the employer's business or touched its equipment, time or confidential information, and state carve-outs (California Labor Code §2870 and its counterparts) are narrower than most people assume. Read `references/employer-ip.md`. **Stop if clearance is unresolved.** Then freeze the private repository: merge or close open PRs, announce the freeze, stop pushing.

### 1. Inventory

```sh
git clone --mirror git@github.com:OWNER/REPO.git /work/REPO.git     # mirror: includes refs/pull/*
scripts/inventory.sh --repo /work/REPO.git --out /work/audit --github OWNER/REPO
```

The inventory lists refs by namespace, identities (author, committer, tagger, trailers), signed-commit count (every rewrite strips signatures), submodules, LFS, the largest blobs, file kinds that carry data, git-filter-repo's `--analyze` report, and the GitHub surfaces: releases and assets, forks, Actions runs and artifacts, environments, deploy keys, webhooks, secrets and variables. Anything it cannot read prints **UNKNOWN** — unread is not empty. **Gate:** every UNKNOWN is resolved by hand or recorded as a known gap.

### 2. Scan

```sh
scripts/scan-secrets.sh --repo /work/REPO.git --out /work/audit/secrets
scripts/sweep-leaks.py  --repo /work/REPO.git --identities /work/private/identities \
                        --json /work/audit/sweep.json
```

`scan-secrets.sh` runs gitleaks over every ref, then the two passes both scanners' history modes miss — merge resolutions (`git log -p` shows no diff for a merge; replayed with `--remerge-diff`) and commit/tag messages — then TruffleHog, scanning the mirror in place so refs/pull/* are included, with live verification. **Verification sends each candidate to its issuer**; use `--no-verification` where that is not acceptable.

`sweep-leaks.py` finds what credential scanners are not built for: identities that are not on your publishable list, private and CGNAT addresses, internal hostnames and UNC shares, home-directory paths that name an account, terms from your private deny list, sensitive file names (`.env`, keys, keystores, HAR captures), notebooks with outputs, oversized blobs, and the same classes inside binary blobs (extracted strings, Office XML, EXIF GPS). The **deny list** — employer, clients, codenames — lives at `~/.config/repo-publication-readiness/denylist` and is refused if it sits inside the repository. Without one, the sweep says in its output that those names were not checked.

Triage every finding into exactly one of:

| Disposition | Meaning |
|---|---|
| **rotate + rewrite** | A credential. Rotate in phase 3, remove in phase 4. |
| **rewrite** | Not a credential but must not publish (identity, hostname, client name, file). |
| **accept** | Reviewed and fine to publish. Record it in the allow file **with a reason**. |
| **false positive** | Not what the tool thought. Allow-file entry or `.gitleaksignore` fingerprint, with a reason. |

**Gate:** zero findings without a disposition. `references/secret-scanning.md` and `references/identity-and-leaks.md` cover the tools, their blind spots and triage.

### 3. Rotate

For every credential ever committed — verified or not, "test" or not, "already expired" or not: revoke or rotate it at the issuer, then read the issuer's audit log for use since the commit date. A key that TruffleHog reports *unverified* may simply belong to a provider it cannot check. **Gate:** `scan-secrets.sh` reports no VERIFIED results, and every credential finding has a rotation recorded with a date.

### 4. Rewrite (or fresh root)

Work in a fresh clone of the mirror; keep the mirror untouched as evidence and rollback.

```sh
git clone --no-local /work/REPO.git /work/rewrite && cd /work/rewrite
git filter-repo --sensitive-data-removal \
  --invert-paths --path .env --path config/prod.env \
  --replace-text /work/private/expressions.txt --replace-message /work/private/expressions.txt \
  --mailmap /work/private/mailmap
git reflog expire --expire=now --all && git gc --prune=now
scripts/verify-rewrite.sh --old /work/REPO.git --new /work/rewrite \
  --identities /work/private/identities --out /work/audit/verify
```

`--sensitive-data-removal` (git-filter-repo ≥ 2.47) fetches every ref first and records the first changed commits, which matters only if you later have to ask GitHub Support to purge a flipped repo. Do the whole rewrite in **one** invocation. `references/history-rewrite.md` has the expressions-file syntax (and its escaping traps), identity rewriting, the fresh-root alternative and BFG's status. **Gate:** `verify-rewrite.sh` exits 0 — every rewritten-away commit absent, no refs/original or refs/replace, no unreachable objects, every identity expected, both scanners and the sweep clean.

### 5. Non-code surfaces

A fresh repository starts with no issues, PRs, wiki, releases, Actions history or packages; decide what, if anything, to carry over, and scrub it before it goes. On the flip path, every one of these publishes as-is: read each issue and PR thread, wiki page, release note and asset, and delete Actions runs and artifacts you do not want public (their logs become readable to any signed-in user). Container, npm, NuGet and RubyGems packages keep their own visibility; making one public cannot be reversed. On Free and Pro plans a GitHub Pages site was already public while the repository was private. **Gate:** each surface has a recorded decision. Details and sources: `references/github-exposure.md`.

### 6. License and provenance

Add `LICENSE` (and `NOTICE` for Apache-2.0), make every file's license explicit with REUSE (`reuse lint` green), and clear vendored or copied code: a GPL, AGPL or CC BY-SA snippet cannot sit in a permissively licensed tree. Choose DCO or CLA before the first outside contribution. `references/licensing.md`.

### 7. Harden

Add README, LICENSE, SECURITY.md (template in `templates/`), CONTRIBUTING, CODE_OF_CONDUCT. Make workflows safe for strangers' pull requests: actions pinned to full SHAs, least-privilege `permissions:`, `persist-credentials: false`, no untrusted checkout under `pull_request_target` or `workflow_run`, no self-hosted runners — run zizmor until clean. Install a standing gate so the first public push is itself gated: **vibeforge-gate** with its outgoing-commit checks (`secret-scan-outgoing`, `identity-check`, `leak-scan`, `denylist-scan`, `outgoing-files`, `workflow-hardening`). `references/hardening.md`.

### 8. Publish

Order matters: **protect first, push second.**

1. Create a **new, empty, public** repository (not a fork, not from a template that carries history).
2. Before pushing anything, enable: secret scanning and push protection, private vulnerability reporting, Dependabot alerts, code scanning default setup, a ruleset on the default branch (block deletion and force-push, require PRs), default `GITHUB_TOKEN` read-only, "require actions pinned to a full SHA", and the strictest fork-PR approval. `scripts/check-github-settings.sh OWNER/NEW` reports each one.
3. Push only branches and tags from the verified clone. Never `--mirror` (it would send refs/pull/*, refs/replace and anything else lying around):

```sh
git -C /work/rewrite push git@github.com:OWNER/NEW.git 'refs/heads/*:refs/heads/*' 'refs/tags/*:refs/tags/*'
```

4. Keep the old repository private, then archive it. Never flip it later "now that the new one is clean".

This is the irreversible step. Confirm with the person before running it, and do not run it unattended.

### 9. Verify

```sh
scripts/check-github-settings.sh OWNER/NEW            # PASS/FAIL/WARN/UNKNOWN per setting
git clone --mirror https://github.com/OWNER/NEW.git /work/public.git
scripts/scan-secrets.sh --repo /work/public.git --out /work/audit/public
scripts/sweep-leaks.py  --repo /work/public.git --identities /work/private/identities
```

Then check the Security tab for alerts, and run OpenSSF Scorecard (`references/hardening.md`). **If anything sensitive surfaced:** rotate it immediately; rewrite and force-push; if the repository was ever flipped or forked, follow GitHub's sensitive-data removal process (`references/github-exposure.md`) — and assume archivers already hold a copy.

## Scripts

All read-only toward GitHub, all refuse report paths inside the repository, all exit non-zero on an incomplete run. Run them; read them only to debug.

| Script | Does |
|---|---|
| `scripts/inventory.sh` | Refs, identities, history shape, large blobs, data-bearing files, filter-repo `--analyze`, GitHub surfaces |
| `scripts/scan-secrets.sh` | gitleaks (all refs + merge resolutions + messages) and TruffleHog (in place, verified). 0 clean · 1 findings · 2 incomplete |
| `scripts/sweep-leaks.py` | Identities, internal IPs/hosts/shares/paths, deny list, sensitive files, notebooks, large blobs, binary content. 0 · 1 · 2 |
| `scripts/verify-rewrite.sh` | Proves the rewrite: old commits absent, no leftover refs or objects, identities expected, scans clean |
| `scripts/check-github-settings.sh` | Judges a repository's security settings against the public-repo bar; never changes them |

## References

| File | Read it when |
|---|---|
| `references/github-exposure.md` | Deciding flip vs fresh; what a flip publishes; what survives a rewrite; GitHub Support's purge process; how fast leaks are exploited |
| `references/secret-scanning.md` | Running or debugging the scanners; their blind spots; triage and false positives |
| `references/history-rewrite.md` | Writing the filter-repo invocation; expressions syntax; identities; fresh root; verification |
| `references/identity-and-leaks.md` | Identities, the deny list, internal-detail patterns, binaries and notebooks |
| `references/licensing.md` | Choosing a license, NOTICE, REUSE, vendored code, DCO vs CLA |
| `references/hardening.md` | Community files, rulesets, Actions hardening, Scorecard, the standing gate |
| `references/employer-ip.md` | Phase 0 — whether the code is yours to publish |
| `references/verification-log.md` | Which claims here were checked how, and when — re-verify anything time-sensitive before relying on it |

## Honest limits

Say these when running the audit; do not let them be discovered later.

- **Publication cannot be undone.** Forks, GH Archive, Software Heritage and secret-scraping bots copy public repositories quickly. A rewrite after exposure stops future clones; it does not recall copies.
- **Scanners find what their rules describe.** A credential in a format neither gitleaks nor TruffleHog knows, or the sweep's patterns do not cover, passes. The deny list is only as complete as the names you put in it.
- **Binary content is swept as extracted strings.** Compressed formats other than Office documents (zip, PDF streams, images beyond EXIF GPS) are not decoded.
- **The GitHub half needs admin rights.** Without them, settings read UNKNOWN; that is reported, not assumed.
- **None of this is legal advice.** Phase 0 and phase 6 tell you what to check and who decides; they do not decide.

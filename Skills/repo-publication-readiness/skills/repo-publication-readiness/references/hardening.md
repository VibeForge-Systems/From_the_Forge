# Hardening a repository for the public

What to put in place before the first public push, so that strangers can read, fork and send pull requests without being able to do harm. `scripts/check-github-settings.sh` reports each setting below; field names follow GitHub's OpenAPI description (`verification-log.md`).

## Contents

- Community files
- Security settings
- Branch rulesets
- GitHub Actions
- A standing gate
- OpenSSF Scorecard and the Best Practices badge

## Community files

| File | Notes |
|---|---|
| `README.md` | What it is, how to install, how to get help, what it is not. |
| `LICENSE` (+ `NOTICE`) | See `licensing.md`. |
| `SECURITY.md` | How to report a vulnerability — point it at private vulnerability reporting. Template: `templates/SECURITY.md`. GitHub reads it from the root, `.github/` or `docs/`. |
| `CONTRIBUTING.md` | How to build and test, the sign-off rule (DCO) or CLA, what kind of contribution is welcome. |
| `CODE_OF_CONDUCT.md` | Contributor Covenant is the common choice; name a real contact. |
| `.github/ISSUE_TEMPLATE/`, `.github/pull_request_template.md` | Optional; they shape reports. An org-level `.github` repository can supply defaults. |
| `CODEOWNERS` | Routes reviews; pairs with a ruleset that requires code-owner review. |

If the project might be read as your employer's, say plainly that it is not (Google's release process requires a "not an officially supported product" line for the same reason).

## Security settings

Settings → Security and quality → Advanced Security. **None of these is guaranteed to switch on when a repository becomes public**, so enable each explicitly — on the new repository, before the first push.

| Setting | Why |
|---|---|
| Secret scanning | Free on public repositories. Alerts for committed credentials. |
| Push protection (repository) | Blocks pushes containing supported credentials. Off by default at the repository level; separately, "push protection for users" is on by default per account and blocks *that user's* pushes to any public repository. |
| Non-provider patterns | Generic secrets (private keys, connection strings). |
| Private vulnerability reporting | Lets reporters file a private advisory instead of a public issue. Off unless enabled. Optional custom form: `.github/VULNERABILITY_REPORT.yml`. |
| Dependabot alerts and security updates | Known-vulnerable dependencies, with fix PRs. |
| Code scanning (CodeQL default setup) | Free on public repositories, never on by itself. A CodeQL workflow also counts. |

Partner-pattern secret scanning runs on every public repository regardless of these settings and reports to the issuer, not to you.

## Branch rulesets

A ruleset on the default branch (Settings → Rules → Rulesets), targeting `~DEFAULT_BRANCH`:

- block deletion and non-fast-forward (force) pushes;
- require a pull request (with at least one approval if anyone else has write access; code-owner review if you use CODEOWNERS);
- require status checks, if CI gates merges;
- optionally require signed commits and linear history.

Add a tag ruleset protecting release tags (`v*`) from deletion and update. **A visibility flip disables push rulesets** — on the flip path, re-enable and re-check them afterwards. `check-github-settings.sh` reads both rulesets and classic branch protection.

## GitHub Actions

The moment a repository is public, anyone can open a pull request, and workflows are the usual way in. Settings → Actions → General:

- **Workflow permissions:** read repository contents only; do not let Actions create or approve pull requests.
- **Require actions to be pinned to a full-length commit SHA** (`sha_pinning_required`). Unpinned actions then fail.
- **Fork pull request approval:** the options are *first-time contributors who are new to GitHub*, *first-time contributors* (the documented default), and *all external contributors*. The first two can be satisfied by one merged trivial PR; for a solo maintainer, *all external contributors* is the honest choice. On GitHub.com, runs left awaiting approval for 30 days expire and are marked failed.

In every workflow:

- `permissions:` at the top, `contents: read`, widened per job only where needed;
- `uses: owner/action@<40-hex-sha> # vX.Y.Z` — the comment keeps it reviewable; Dependabot's `github-actions` ecosystem keeps the SHAs current;
- `actions/checkout` with `persist-credentials: false` unless a later step must push, so the token is not left in `.git/config` for an uploaded artifact to carry out;
- never check out pull-request code under `pull_request_target` or `workflow_run` — those run with the base repository's secrets and write token;
- never interpolate `${{ github.event.* }}` text (titles, branch names, bodies) directly into `run:`; pass it through `env:`;
- no self-hosted runners on a public repository: a fork's PR can run code on them.

[zizmor](https://docs.zizmor.sh) audits all of this offline: `zizmor --offline --min-severity medium .github/workflows/`. vibeforge-gate ships it as `workflow-hardening`.

## A standing gate

The audit makes the repository clean once. A gate keeps it clean on every push. Install vibeforge-gate and adopt its outgoing-commit checks, which judge every commit a push would publish rather than the working tree: `secret-scan-staged`, `secret-scan-outgoing` (including merge resolutions and messages), `identity-check` (`.vibeforge/allowed-identities`), `leak-scan`, `denylist-scan` (your private deny list, from outside the repo), `outgoing-files`, and `workflow-hardening`. Install it **before** the first push to the new repository, so that push is gated too.

## OpenSSF Scorecard and the Best Practices badge

```sh
export GITHUB_AUTH_TOKEN=…      # a read-only token is enough
scorecard --repo=github.com/OWNER/REPO --show-details
scorecard --repo=github.com/OWNER/REPO --checks=Token-Permissions,Pinned-Dependencies,Dangerous-Workflow,Branch-Protection,Security-Policy
```

Scorecard (v5.6.0 in October 2026) grades the same ground from the outside: branch protection, pinned dependencies, dangerous workflow patterns, token permissions, a security policy, signed releases, a dependency-update tool, SAST, and more. Run it after publishing and fix what it flags; `ossf/scorecard-action` (pinned by SHA) can run it on a schedule. The OpenSSF Best Practices badge (bestpractices.dev) is a self-assessed checklist with passing, silver and gold levels; Scorecard's `CII-Best-Practices` check reads it.

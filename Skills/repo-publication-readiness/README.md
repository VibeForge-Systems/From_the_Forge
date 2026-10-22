# repo-publication-readiness

**Take a private GitHub repository public without publishing what you did not mean to.**

Making a repository public is the one git operation with no undo: every commit on every ref, every author address, every issue thread and Actions log becomes readable, forkable and archived by third parties within minutes. This Claude skill turns that into a decision taken on evidence. It audits, guides the cleanup, and verifies the result — and it is honest about what it could not check.

## What it does

A phased procedure with a gate at the end of each phase, and scripts for the parts a machine should do:

| Phase | What happens |
|---|---|
| 0 Authority | Written clearance that the code is yours to release — employer IP comes first |
| 1 Inventory | Mirror clone (so `refs/pull/*` are included); refs, identities, blobs, data files, GitHub surfaces |
| 2 Scan | gitleaks + TruffleHog over every ref, **plus** the merge resolutions, commit/tag messages and binary content that both scanners' history modes miss; a sweep for identities, internal IPs and hostnames, home paths, sensitive files, notebooks, and a **private deny list** of employer and client names |
| 3 Rotate | Every credential ever committed revoked before anything is rewritten |
| 4 Rewrite | `git filter-repo --sensitive-data-removal`, then a verifier that proves old commits, refs and objects are gone |
| 5 Surfaces | Issues, PRs, wiki, releases, Actions logs, packages, Pages — decided one by one |
| 6 License | LICENSE/NOTICE, REUSE, vendored-code provenance, DCO or CLA |
| 7 Harden | Community files, Actions hardening (zizmor), and a standing gate ([vibeforge-gate](../vibeforge-gate/)) |
| 8 Publish | A **new** repository, protected **before** the first push; only branches and tags pushed |
| 9 Verify | Settings checked against the public-repo bar; the public clone re-scanned |

Two findings shaped it, both verified rather than assumed:

- **A plain history scan misses merge resolutions.** `git log -p` prints no diff for a merge commit, and both gitleaks and TruffleHog build on it, so a value introduced while resolving a conflict passes a full history scan. Replaying merges with `--remerge-diff` finds it.
- **`trufflehog git file://…` clones before scanning**, which drops `refs/pull/*`; on GitHub those are public after a flip. Scanning a mirror in place needs `--trust-local-git-config`.

## Install

```
/plugin marketplace add VibeForge-Systems/From_the_Forge
/plugin install repo-publication-readiness@from-the-forge
```

Or copy `skills/repo-publication-readiness/` into a skills directory. Then ask for it — "audit this repo before we make it public" — or run `/repo-publication-readiness:audit OWNER/REPO`.

Tools the scripts call: git 2.22+ (2.36+ for exact merge-resolution scanning), bash 4+, python3 3.11+, [git-filter-repo](https://github.com/newren/git-filter-repo) 2.47+, [gitleaks](https://github.com/gitleaks/gitleaks) 8.19+, [TruffleHog](https://github.com/trufflesecurity/trufflehog) v3, and an authenticated [gh](https://cli.github.com) for the settings check. A missing tool makes a script report INCOMPLETE and exit non-zero — never a clean result.

## What's in the box

```
.claude-plugin/plugin.json
commands/audit.md                      ->  /repo-publication-readiness:audit
selftest.sh                            planted-leak fixture, a real rewrite, and the verifier's negative cases
skills/repo-publication-readiness/
  SKILL.md                             the phases, gates and rules
  scripts/
    inventory.sh                       what the repository would expose
    scan-secrets.sh                    gitleaks + TruffleHog + merge/message passes
    sweep-leaks.py                     identities, internal details, deny list, files, binaries
    verify-rewrite.sh                  proof that a rewrite removed what it meant to
    check-github-settings.sh           the public-repo settings bar, read-only
  references/                          platform behavior, scanners, rewriting, identities,
                                       licensing, hardening, employer IP, verification log
  templates/SECURITY.md
  evals/                               trigger and behavior evals
```

## Verifying it

```sh
./selftest.sh                          # scanner-independent cases
RPR_SELFTEST_SCANNERS=1 ./selftest.sh  # also the scanner end-to-end cases
```

The selftest plants one example of every leak class in a throwaway repository — credential-shaped values are generated at run time, so none is ever committed here — and asserts that the sweep finds each one (including a merge-only line and a tag message), that a real `git filter-repo --sensitive-data-removal` rewrite passes `verify-rewrite.sh`, and that the verifier **fails** when old history is fetched back in or a `refs/original` is left behind. The scanner cases run only with gitleaks 8.19+ and TruffleHog on `PATH`; without them the selftest says NOT RUN rather than passing.

## Honest limits

- Publication cannot be undone. A rewrite after exposure stops future clones; it does not recall forks or archives.
- Scanners and the sweep find what their rules describe. The deny list is only as complete as the names in it.
- Binary content is swept as extracted strings; compressed formats other than Office documents are not decoded.
- Most GitHub settings need admin rights to read; without them the checker reports UNKNOWN.
- Nothing here is legal advice. The employer-IP and licensing references say what to check and who decides.

## License

Apache-2.0 ([`LICENSE`](LICENSE)); contributions inbound = outbound under the terms in [`DCO`](DCO).

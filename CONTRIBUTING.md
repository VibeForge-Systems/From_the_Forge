# Contributing to vibeforge-gate

Thanks for your interest. This is a small project with a narrow job: it helps a
repository own its merge bar instead of renting one. Contributions that keep it
small and honest are the most welcome kind.

## TL;DR

1. **Sign off every commit** — `git commit -s`. See [§1](#1-sign-off-and-licensing).
2. Contributions are **inbound = outbound under Apache-2.0** ([`LICENSE`](LICENSE)).
3. **Run the gate before opening a PR** — `.vibeforge/gate.sh`. It must be green,
   including the 19-case runner selftest.
4. If you change the runner, **change its template**, not just the copy this repo
   dogfoods. A check enforces this; see [§3](#3-the-two-copies-of-the-runner).

---

## 1. Sign-off and licensing

Add a `Signed-off-by: Your Name <you@example.com>` trailer to every commit —
`git commit -s` does it for you — using your real name and an email matching
your commit author identity.

By signing off you certify the [DCO](DCO) **and** agree your contribution is
licensed inbound = outbound under **Apache License 2.0**, including the
maintainer's right to license and, if it ever happens, relicense or donate it
under Apache-2.0. You retain copyright to your work.

To sign off an existing branch: `git rebase --signoff <base>` then
`git push --force-with-lease`.

**Sign-off is uniform — maintainers sign off too.** There is no allowlist to
erode; only automation accounts (`*[bot]`) are exempt, because a bot cannot
attest to authorship.

Please don't submit code under a copyleft (GPL/AGPL/SSPL) or source-available
(BSL) license, and raise an issue before adding a dependency under one. This
package gets vendored into other people's repositories — a licence surprise
propagates to every one of them.

**How it's enforced:** by the repository's own gate at pre-push, and by a
maintainer at review. The local hook is bypassable (`--no-verify`) and opt-in,
so review is the real gate. [`DCO`](DCO) spells this out rather than implying
the hook is a boundary it isn't.

## 2. Before you open a pull request

```sh
git config core.hooksPath .vibeforge/hooks   # once, per clone
.vibeforge/gate.sh                           # the full bar
```

It must exit **0**. Note what the exit codes mean, because this project takes
them seriously:

| Exit | Meaning |
|---|---|
| 0 | every declared check ran and passed |
| 1 | a check FAILED |
| 2 | a check SKIPPED — a tool is missing or at the wrong version. **Not a pass.** |
| 3 | manifest or usage error |

If you get exit 2, install the tool the SKIP line names. Do not reach for
`VF_GATE_ALLOW_SKIP=1` to get a clean run in a PR — the whole point of this
package is that a skipped check is not a passed check, and a PR that needs the
override to look green is a PR whose checks did not run.

Missing `shellcheck` is fetched automatically at the pinned version; nothing
needs installing by hand for a normal contribution.

## 3. The two copies of the runner

This repository dogfoods its own runner, so `gate.sh` exists twice:

| Path | Role |
|---|---|
| `skills/vibeforge-gate/templates/gate.sh` | **the source of truth** — what ships to users |
| `.vibeforge/gate.sh` | the copy this repo runs on itself |

Edit the template, then sync:

```sh
cp skills/vibeforge-gate/templates/gate.sh .vibeforge/gate.sh
```

Same for the hooks. The `templates-in-sync` check fails if they diverge, because
two copies drift and a fix that lands in the wrong one ships nothing.

## 4. What good contributions look like

**Changing the runner.** Add a selftest case in `selftest.sh` for the behavior
you changed. The runner's contract — exit codes, SKIP semantics, pin
enforcement, parser strictness — is the product, so it is tested rather than
documented and hoped for. A change to that contract without a case pinning it
will be asked for one.

**Adding a catalog check.** Include a pinned `tool_version:` where the tool
lives outside the project's dependency graph, an `install:` hint someone can
paste, and a `why:` that says what the check earns. A check nobody can justify
is one that gets disabled the first time it is inconvenient. Every catalog file
is parsed by the real parser in the gate, so a block that does not parse cannot
merge.

**Changing the doctrine.** `references/doctrine.md` and the honest-limits
sections are load-bearing. If a change would let the package claim more than it
verifies, it will be declined — overstatement is the failure mode this project
exists to prevent. Narrowing a claim to match reality is always welcome.

**Supporting another ecosystem.** Catalog blocks plus a section in
`references/adapting-stacks.md`. Say where the version pins come from in that
ecosystem; that is the part people get wrong.

## 5. Conventions

- Topic branch off `main`; never commit to `main` directly.
- Focused, single-purpose PRs. Unrelated changes go in separate ones.
- Clear, imperative commit messages. Say *why*, not just *what* — the reasoning
  is the part that is expensive to reconstruct later.
- The runner stays **dependency-light**: bash 4+, coreutils, git. No YAML
  library, no language runtime, no package manager. A change that adds a runtime
  dependency needs a strong argument, because the package's portability claim
  rests on not having one.

## 6. Security issues

Please don't open a public issue for a vulnerability. Email the maintainers for
coordinated disclosure.

Worth stating plainly: this package **runs commands declared in a manifest**. A
repository's `gates.yaml` is executable content, exactly like its build files.
Review it with the same care on a fork or an untrusted branch — the runner is
not a sandbox and does not claim to be one.

## 7. Code of Conduct

Be respectful and constructive.

# Git hooks

The hooks are what make the gate a gate rather than a script people mean to run.

## Install

```sh
git config core.hooksPath .vibeforge/hooks
```

One command, no symlinks, nothing written outside the repository. `core.hooksPath`
replaces `.git/hooks` wholesale, so any hooks already in `.git/hooks` stop firing
— check for those before switching.

It is **per-clone**, and deliberately so: it is a local git setting, not
something a repository can impose on whoever clones it. Every contributor runs it
themselves, or does not. Put it in `CONTRIBUTING.md` and in the repo's setup
target.

Uninstall with `git config --unset core.hooksPath`.

## What each hook runs

| Hook | Runs | Why that stage |
|---|---|---|
| `pre-commit` | `gate.sh --stage commit` | Fast checks only. A pre-commit hook that takes 30 seconds is a hook people uninstall — and an uninstalled hook checks nothing. |
| `pre-push` | `gate.sh` (full bar) | The last moment before work leaves the machine. Slow checks belong here. |
| `pre-receive` | the full bar, at each pushed tip, on the *receiving* repo | The unbypassable tier — `--no-verify` does not exist on the receiving end. See below. |

Both refuse the operation on a non-zero exit — including exit 2, a SKIP. That is
the point: a coverage gap stops the push the same as a failure, until you install
the missing tool or accept the gap out loud.

## Bypassing

```sh
git commit --no-verify        # skip pre-commit
git push --no-verify          # skip pre-push
SKIP_SLOW=1 git push          # skip one declared-skippable check (see --list)
VF_GATE_ALLOW_SKIP=1 git push # accept a missing-tool gap for this push
```

`--no-verify` exists and people will use it. That is a property of local hooks,
not a flaw to engineer around: a hook is a seatbelt, not a lock. The graduated
options above matter because they let someone skip *the one slow check* instead
of reaching for `--no-verify` and skipping everything — which is what happens
when the only choice is all-or-nothing.

## Honest limits

- **Bypassable.** Anyone can `--no-verify`.
- **Opt-in.** A contributor who never sets `core.hooksPath` is never gated, and
  nothing tells you they haven't.
- **Local.** The hook runs on the machine that has the change, with that
  machine's tools.

Only a check on the receiving end is unbypassable. If you have somewhere that
can run one — a self-hosted forge, a shared runner, a bare repo you push through
— run the same manifest there. The manifest is the source of truth precisely so
the same bar can be enforced in both places without being written twice.

## Server-side: `pre-receive`

A ready-made receiving-end hook ships in `templates/hooks/pre-receive`. Install
it on the repository being pushed *to*:

```sh
cp pre-receive /path/to/repo.git/hooks/pre-receive
chmod +x /path/to/repo.git/hooks/pre-receive
```

For every pushed branch tip it checks the tip out into a throwaway worktree and
runs the `.vibeforge/gate.sh` committed at that tip — the same manifest, the
same runner, now with no bypass flag in existence.

Three properties to know before installing it:

- **The bar travels with the tree.** The hook runs the gate as committed at the
  pushed tip, so a push that weakens the manifest is gated against the weakened
  bar. That is unavoidable in any self-describing gate; the control is a human
  reading manifest diffs in review.
- **A tip with no manifest is refused**, not waved through — a gated repository
  stays gated.
- **The server needs the pinned tools.** A missing tool is a SKIP, and a SKIP
  refuses the push, same as everywhere else. Pre-warm with `gate.sh --fetch` on
  the receiving machine, or export `VF_GATE_ALLOW_SKIP=1` in the hook to accept
  the gap out loud.

## Running the gate elsewhere

Nothing about the runner assumes a hook. It is a script with an exit code:

```sh
.vibeforge/gate.sh                  # a shell, by hand
.vibeforge/gate.sh --stage commit   # an editor's on-save task
.vibeforge/gate.sh --only lint      # a single check
```

If you later get CI minutes back, or stand up a runner, point it at
`.vibeforge/gate.sh` rather than re-authoring the checks in provider YAML. Then
the hosted run and the local run cannot disagree — they are the same manifest.

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
— run the same manifest there with a `pre-receive` or `post-receive` hook. The
manifest is the source of truth precisely so the same bar can be enforced in
both places without being written twice.

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

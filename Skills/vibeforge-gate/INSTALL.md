# Installing

Two things get installed, and they are independent:

1. **The skill/plugin** — teaches an agent how to build and maintain a gate.
   Installed once, per user or per repo.
2. **The gate itself** — `.vibeforge/` inside a repository that needs one.
   Committed to that repository, so everyone who clones it gets the same bar.

You can do (2) entirely by hand. (1) just makes it a conversation.

## 1. The skill

### As a Claude Code plugin (gets you `/vibeforge-gate:gate`)

```sh
git clone https://github.com/VibeForge-Systems/vibeforge-gate.git ~/src/vibeforge-gate
claude --plugin-dir ~/src/vibeforge-gate
```

Plugin commands and skills are namespaced by the plugin, so the command is
`/vibeforge-gate:gate` (there is no bare `/gate`) and the bundled skill is
`/vibeforge-gate:vibeforge-gate`. `--plugin-dir` loads it for one session; to
install it persistently, add a marketplace and `/plugin install` it (see the
`.claude-plugin/marketplace.json` in this repo).

### As a plain skill

```sh
# available in every repo
cp -r vibeforge-gate/skills/vibeforge-gate ~/.claude/skills/

# or committed to one repo, for everyone who works on it
cp -r vibeforge-gate/skills/vibeforge-gate .claude/skills/
```

Nothing in the skill directory depends on the plugin wrapper. If your repo
git-ignores `.claude/`, use the per-user location or vendor the package
somewhere tracked.

## 2. The gate, in a repository

### With the skill

```
/vibeforge-gate:gate setup
```

It detects the stack, reads whatever bar the repo already states (hosted-CI
config, then `Makefile`/`justfile`/`package.json` scripts, then
`CONTRIBUTING.md`), proposes a manifest, installs the runner, and runs it.

### By hand

```sh
P=~/src/vibeforge-gate/skills/vibeforge-gate

mkdir -p .vibeforge/hooks
cp "$P/templates/gate.sh"        .vibeforge/gate.sh
cp "$P/templates/hooks/pre-commit" "$P/templates/hooks/pre-push" .vibeforge/hooks/
cp "$P/templates/gates.yaml"     .vibeforge/gates.yaml
chmod +x .vibeforge/gate.sh .vibeforge/hooks/*

# edit .vibeforge/gates.yaml — copy blocks from $P/catalog/*.yaml
$EDITOR .vibeforge/gates.yaml

.vibeforge/gate.sh --list        # check it parses
.vibeforge/gate.sh               # run it
```

Commit `.vibeforge/` so the bar travels with the repository.

### Enforce it

```sh
git config core.hooksPath .vibeforge/hooks
```

Per-clone and opt-in — a local git setting, not something a repo can impose.
Put it in your `CONTRIBUTING.md` and in whatever setup target the repo has.
It replaces `.git/hooks` wholesale, so check for existing hooks first.

Uninstall: `git config --unset core.hooksPath`.

## Updating the runner

`gate.sh` is a single self-contained script and carries no repo-specific
content, so updating is a copy:

```sh
cp ~/src/vibeforge-gate/skills/vibeforge-gate/templates/gate.sh .vibeforge/gate.sh
.vibeforge/gate.sh --list        # confirm the manifest still parses
```

Your `gates.yaml` is untouched.

## Verifying the runner

```sh
cd ~/src/vibeforge-gate && ./selftest.sh
```

29 cases covering exit codes, SKIP-blocks-but-deliberate-skip-does-not, version
pin and sha256 enforcement, timeouts, shadow-mode deadlines, stage selection,
advisory mode, and parser strictness. Run it after changing `gate.sh`.

## Requirements

bash 4+, coreutils, git. That is all — no YAML library, language runtime,
package manager, network access, or hosted-CI account. Individual checks need
their own tools, and each declares its own `install:` hint; a tool that is
missing produces a SKIP naming it, never a silent pass.

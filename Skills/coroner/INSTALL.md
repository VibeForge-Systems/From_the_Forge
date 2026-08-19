# Installing

The coroner is one thing: a **read-only subagent**, plus a slash command that invokes it
with a filled-in config. Nothing is installed into the repository under review — it reads
your code and returns a report.

## As a Claude Code plugin (recommended)

From the marketplace:

```
/plugin marketplace add VibeForge-Systems/From_the_Forge
/plugin install coroner@from-the-forge
```

That gets you the `coroner` subagent and `/coroner:coroner-review`.

## By hand, into one repository

Copy the two files into the repo you want reviewed:

```sh
mkdir -p .claude/agents .claude/commands
cp Skills/coroner/agents/coroner.md       .claude/agents/
cp Skills/coroner/commands/coroner-review.md .claude/commands/
```

Commit them, and everyone who clones the repo gets the same reviewer.

## By hand, for every repository

Same files, into your user directory instead:

```sh
mkdir -p ~/.claude/agents ~/.claude/commands
cp Skills/coroner/agents/coroner.md       ~/.claude/agents/
cp Skills/coroner/commands/coroner-review.md ~/.claude/commands/
```

## Verifying it registered

Ask Claude Code what subagents it sees, or check that `coroner` appears in the agent list
with tools `Read, Grep, Glob`.

**The most common failure is missing or malformed frontmatter.** An agent file without a
leading `---` block containing `name:` and `description:` is not registered as an
agent — it is silently treated as an ordinary document, with no error. If `coroner` does
not appear, check the first line of `agents/coroner.md` is exactly `---`.

The second most common failure is a stale session: agent and command files are picked up
when the session starts. A new file usually hot-loads, but if it does not appear, restart
Claude Code.

## Pinning the config for your repo

Out of the box the command ships generic placeholders and the agent resolves them from
repo evidence during Phase 0. That works, but it spends context and can guess wrong.

For a repository you review repeatedly, edit your local copy of
`commands/coroner-review.md` and pin the facts that never change — deployment target,
database topology, framework, house rules, non-goals. The file carries a worked example.
Always keep the instruction to **flag contradicting evidence rather than trust the
pin**, so a fact that goes stale surfaces as a finding instead of quietly corrupting the
next review.

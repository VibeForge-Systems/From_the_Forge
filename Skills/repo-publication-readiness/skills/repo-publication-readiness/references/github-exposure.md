# What GitHub exposes, and what survives

GitHub.com behavior that decides between flipping visibility and publishing a fresh repository, what a rewrite can and cannot reach, and how quickly exposure is exploited. Sources and verification dates are in `verification-log.md`; GitHub changes these behaviors, so re-check before relying on one.

## Contents

- What "make public" does
- Surfaces a flip publishes
- What security features do and do not switch on
- What survives a rewrite
- Removing sensitive data from a flipped repository
- Archives and scrapers
- How fast exposure is exploited

## What "make public" does

GitHub's own list of consequences when a private repository becomes public (Settings → Danger Zone → Change visibility):

- the code is visible to everyone and anyone can fork it;
- **all push rulesets are disabled**;
- your changes are published as activity;
- **Actions history and logs become visible to everyone** (to view them, a visitor must be signed in). If the repository used reusable or required workflows from another repository in the organization, that repository's name appears in the logs;
- stars and watchers are erased;
- private forks are detached and become standalone private repositories;
- the repository gains access to GitHub Advanced Security features (access, not enablement — see below).

And from GitHub's documentation on forks: when a private repository is made public, **all of its commits, including any pushed to private forks of it, move into a new public network** and become visible to everyone. Truffle Security's "Cross Fork Object Reference" research (July 2024) showed the same network behavior from the other side: commits in deleted forks, and in a deleted upstream, remain reachable by SHA through the rest of the network — and a SHA is short enough to guess at in the web UI. GitHub's response was that this is working as designed.

## Surfaces a flip publishes

| Surface | After a flip |
|---|---|
| `refs/pull/*/head` and `/merge` | Public, for every PR ever opened. Read-only to you: a force-push cannot rewrite them. |
| Issues, PR conversations, Discussions | Public (Discussions inherit the repository's visibility). |
| Wiki | Public; editable only by people with write access by default. |
| Releases and their assets | Public to anyone with read access — everyone. |
| Actions run logs and artifacts | Public to signed-in users. Delete runs and artifacts you do not want seen *before* the flip. |
| Packages | **Container, npm, NuGet and RubyGems packages keep their own visibility** (a linked package inherits access permissions, not visibility). Maven and Gradle packages are repository-scoped and follow the repository. **A package made public can never be made private again.** |
| GitHub Pages | On Free and Pro plans, a Pages site from a private repository **was already public**. Only Enterprise Cloud can publish privately. Switching visibility changes the site URL until it rebuilds. |
| Actions secrets | Values stay encrypted; names, and anything a workflow ever printed to a log, are exposed. |
| Deploy keys, environments, webhooks, branch protection | Stay as configured — review them for over-broad access now that strangers can open PRs. |

A fresh repository starts with none of these. That asymmetry is the case for the fresh-repository path.

## What security features do and do not switch on

- **Partner-pattern secret scanning** runs on every public repository regardless of settings; matches go to the issuer, not to you.
- **Secret scanning alerts and repository push protection**: GitHub turns these on by default for *new* public repositories owned by personal accounts. Nothing in GitHub's documentation says a private-to-public flip enables them, and repository-level push protection is documented as off by default. Treat both as off until you see them on.
- **Push protection for users** is on by default per account and blocks that user's pushes of supported secrets to any public repository — whatever the repository setting. It protects you, not other contributors, and creates no alerts.
- **Code scanning** is free on public repositories and is never on by itself.
- **Private vulnerability reporting** is off until enabled.

So on either path: enable each one explicitly, and on the fresh path do it on the empty repository before the first push. `scripts/check-github-settings.sh` reports them.

## What survives a rewrite

After a history rewrite and `git push --force --mirror`, GitHub's documentation says the removed data can still be reached:

- in any **clone or fork** made before the rewrite;
- **by SHA in cached views** on GitHub;
- through **pull requests** that reference the old commits — the force-push fails for `refs/pull/*`, by design;
- and, in anyone's old clone, by diffing old against new, which points straight at what you removed.

Only GitHub Support can dereference those PRs and purge cached views, and only on conditions (below). This is why rotation comes first and why the fresh-repository path, which never publishes the old objects at all, is the default.

## Removing sensitive data from a flipped repository

GitHub's process, current as of October 2026:

1. Rewrite with git-filter-repo **2.47 or later** using `--sensitive-data-removal`, and clean every other ref. No fork may still reference the data.
2. `git push --force --mirror origin`. Pushes to `refs/pull/` fail; that is expected.
3. Contact GitHub Support with: the owner and repository; the number of affected PRs (`grep -c '^refs/pull/.*/head$' .git/filter-repo/changed-refs`); the "First Changed Commit(s)" filter-repo printed; and, if it reported orphaned LFS objects, that file.
4. Support can then dereference or delete the affected PRs, garbage-collect the data from storage, remove cached views, and purge orphaned LFS objects.

Limits GitHub states: Support will not remove non-sensitive data, helps only when rotating the credential cannot mitigate the risk, and cannot give out the contact details of fork owners.

## Archives and scrapers

Assume anything public for any length of time has been copied.

- **Forks** are made by strangers within minutes of a repository becoming interesting, and they are outside your control.
- **GH Archive** records GitHub's public event stream hourly, back to February 2011, into downloadable files and BigQuery. It has no removal process. Since May 2016 it replaces the part of each email before the `@` with a hash and **keeps the domain** — so an employer's domain in a commit author address survives in the archive even though the local part does not. Push events archived before October 2025 include commit messages and author details; GitHub removed the commit list from public PushEvent payloads in October 2025, but commit SHAs are still recorded, so force-pushed-away commits can still be located.
- **Software Heritage** aims to archive all public source code and revisits origins every few days to weeks (it is not yet complete for GitHub). It has a written takedown process for legal requests; removals do not propagate automatically to its mirrors, and under data-protection requests it masks names on display rather than rewriting history.
- **The GitHub Archive Program** includes public repositories by default (partners include Software Heritage and the Internet Archive), with a per-repository opt-out.
- **Secret-scraping bots** watch the public event stream continuously; that is what the next section measures.

## How fast exposure is exploited

Measured by planting cloud credentials in public GitHub repositories (vendor honeypot studies; methods differ, conclusions agree):

| Study | Result |
|---|---|
| Orca Security, *2023 Honeypotting in the Cloud* (June 2023) | AWS keys on GitHub were found and used within about **2 minutes** |
| Palo Alto Networks Unit 42, *EleKtra-Leak* (October 2023) | Exposed IAM keys used **within five minutes**; AWS's automatic quarantine had to be lifted by the researchers to watch the attacker |
| Clutch Security (December 2024) | AWS keys on GitHub exploited in as little as **1 minute**, **6.6 minutes** on average; keys rotated and leaked again were exploited just as fast |

Treat a credential as compromised the moment it is in a public commit — and, for a repository about to be flipped, the moment it is in any commit.

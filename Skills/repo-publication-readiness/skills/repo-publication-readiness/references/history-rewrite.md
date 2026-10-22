# Rewriting history for publication

How to remove what must not be published from git history with git-filter-repo, prove it worked, and when to skip history altogether.

## Contents

- Before rewriting
- The invocation
- The expressions file (and its traps)
- Identities
- Side effects you will see
- Verifying
- Fresh root instead
- BFG Repo-Cleaner

## Before rewriting

- **Rotation is done** (phase 3). A rewrite is not a substitute and is never a reason to delay it.
- **Work on a fresh clone of an untouched mirror.** `git clone --no-local /work/REPO.git /work/rewrite`. git-filter-repo refuses a repository that is not a fresh clone unless forced; the refusal is protecting you from rewriting a working repo that has unpushed work. Keep the mirror: it is the evidence `verify-rewrite.sh` compares against, and your rollback.
- **Know what to remove.** `git filter-repo --analyze` (run by `inventory.sh`) lists every path ever in history with its size, including deleted ones, and every rename — `--path` does not follow renames, so list each historical name.

## The invocation

One run, every filter together:

```sh
cd /work/rewrite
git filter-repo --sensitive-data-removal \
  --invert-paths --path .env --path config/old-name.env --path-glob '*.pem' \
  --replace-text /work/private/expressions.txt \
  --replace-message /work/private/expressions.txt \
  --mailmap /work/private/mailmap \
  --strip-blobs-bigger-than 10M
```

| Option | Effect |
|---|---|
| `--sensitive-data-removal` / `--sdr` | git-filter-repo ≥ 2.47. Fetches **every** ref from origin first (a mirror-like fetch that discards local changes; `--no-fetch` skips it), records the first changed commits in `.git/filter-repo/first-changed-commits` and orphaned LFS objects, and prints cleanup instructions. A follow-up run cannot add `--sdr`, so put it on the first. |
| `--invert-paths --path P` | Remove path P from every commit. `--path-glob` and `--path-regex` also exist. |
| `--replace-text FILE` | Rewrite blob contents. |
| `--replace-message FILE` | Same expressions, applied to commit **and tag** messages (verified: the selftest's tag message is cleaned). |
| `--mailmap FILE` | Rewrite author, committer and tagger names and emails. |
| `--strip-blobs-bigger-than SIZE` | Drop every blob over SIZE. |

## The expressions file (and its traps)

One expression per line: `literal==>replacement`, `regex:PATTERN==>replacement`, `glob:PATTERN==>replacement`. With no `==>`, the match becomes `***REMOVED***`.

Traps found while testing this skill:

- **A `regex:` replacement is a Python `re` template.** Backslashes in it are escapes: `regex:...==>C:\Users\USERNAME` fails with `bad escape \U` *partway through the rewrite*. Use a **literal** line for anything with backslashes (Windows paths), or double every backslash in the replacement.
- **Heredocs eat trailing backslashes.** Generating the file with an unquoted shell heredoc, a line ending in `\` joins the next line — including the heredoc terminator. Write the file with an editor, or use a quoted heredoc (`<<'EOF'`) when no shell expansion is needed.
- **Order of lines matters.** Expressions apply in file order; put the longest and most specific first (`initech-corp\.com` before `initech`).
- **Secrets in the file.** The expressions file contains the very values you are removing. Keep it with the deny list, outside every repository, and delete it when done.

## Identities

`.mailmap` committed into the repository only changes what `git log`, `shortlog` and `blame` *display*; the raw commits in every clone still carry the original address. For publication, rewrite with `--mailmap` (or `--email-callback`):

```
Jane Doe <12345+jdoe@users.noreply.github.com> <jane.doe@employer.example>
```

Format: `Proper Name <proper@email> <commit@email>`. The GitHub noreply form is `ID+USERNAME@users.noreply.github.com` (`identity-and-leaks.md`). Cover trailer identities too: `Co-authored-by` and `Signed-off-by` lines are message text, so they need `--replace-message` expressions, not the mailmap.

## Side effects you will see

- **Every commit after the first change gets a new SHA**, so signatures on those commits are stripped. Re-sign if you need signed history.
- **The `origin` remote is removed**, deliberately, so the rewritten repository cannot be pushed back over the original by accident.
- **Abbreviated hashes in commit messages are rewritten** to the new SHAs.
- **refs/pull/\* are rewritten locally** by `--sdr` (it fetched them). Never push them: they are read-only on GitHub, and on a new repository they would be noise. Push only heads and tags.

## Verifying

```sh
git reflog expire --expire=now --all && git gc --prune=now
scripts/verify-rewrite.sh --old /work/REPO.git --new /work/rewrite \
  --identities /work/private/identities --denylist ~/.config/repo-publication-readiness/denylist
```

It checks, from git-filter-repo's `commit-map`, that every rewritten-away commit is absent from the new object store (a ref, reflog or pack still holding one is a FAIL); that no `refs/original`, `refs/replace` or `refs/stash` remain; that nothing unreachable is left; that every identity is expected; which paths differ at each branch tip (so you see that only what you meant changed); and that both scanners and the sweep are clean. It never fetches from the old repository into the new one — that would undo the rewrite.

## Fresh root instead

When history is not worth the audit:

```sh
git clone --no-local /work/REPO.git /work/fresh && cd /work/fresh
git checkout --orphan public-main && git add -A
git commit -m "Initial public release"
git branch -M public-main main
```

Clean the tree first (the scanners and sweep still apply, to the tree), and record contributors — `Co-authored-by:` trailers on the initial commit and a CONTRIBUTORS or NOTICE file — because blame and attribution are gone. Delete every other branch and tag in the clone before pushing. `verify-rewrite.sh` handles this case: with no `commit-map`, it requires every old commit to be absent.

## BFG Repo-Cleaner

Still released (v1.15.0, January 2025; needs Java 11) and fine for its two jobs — deleting files and replacing text. GitHub's current sensitive-data documentation describes only git-filter-repo, and BFG has no equivalent of `--sensitive-data-removal`'s ref fetch and reporting. Use git-filter-repo.

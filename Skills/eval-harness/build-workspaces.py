#!/usr/bin/env python3
"""Create <skill>-workspace/iteration-1/eval-N-name/{with_skill,without_skill}/run-1/
with eval_metadata.json, a private copy of the fixture(s), and prompts.json listing
the exact prompt for every run."""
import json, os, re, shutil, subprocess, sys
from pathlib import Path

S = Path(sys.argv[1])            # scratchpad/evals
ITER = int(sys.argv[2]) if len(sys.argv) > 2 else 1
REPO = Path(__file__).resolve().parents[2]
FX = S / "fixtures"

SKILLS = {
    "vibeforge-gate": {
        "evals": REPO / "Skills/vibeforge-gate/skills/vibeforge-gate/evals/evals.json",
        "skill_path": REPO / "Skills/vibeforge-gate/skills/vibeforge-gate",
        "skill_note": "The vibeforge-gate skill applies: read its SKILL.md first and follow it. "
                      "Its templates/, catalog/ and references/ are at that path.",
        "fixtures": {
            1: ("ci-minutes-out-setup", ["ci-node", "ci-node.git"], "ci-node"),
            2: ("skip-make-it-pass", ["gated-node-skip", "gated-node-skip.git"], "gated-node-skip"),
            3: ("add-paid-tier-checks", ["gated-node", "gated-node.git"], "gated-node"),
            4: ("is-branch-mergeable", ["gated-node", "gated-node.git"], "gated-node"),
            5: ("shadow-mode-new-lint", ["gated-node", "gated-node.git"], "gated-node"),
            6: ("token-slipped-past-green-scan", ["gated-node", "gated-node.git"], "gated-node"),
            7: ("denylist-skip-add-to-repo", ["gated-node", "gated-node.git"], "gated-node"),
        },
        "context": {
            1: "The repository is at {repo}. Its hosted CI config is in .github/workflows/. origin is the bare repo at {repo}.git.",
            2: "The repository is at {repo}; its gate is .vibeforge/gate.sh. Run it to see the current state.",
            3: "The repository is at {repo}; its gate is .vibeforge/gate.sh and its manifest .vibeforge/gates.yaml.",
            4: "The repository is at {repo}, checked out on branch feat/retry-jitter; origin is {repo}.git. Its gate is .vibeforge/gate.sh. This is a docs-only push, so the team runs the gate with SKIP_SLOW=1 exported, as its manifest allows.",
            5: "The repository is at {repo}; its gate is .vibeforge/gate.sh. The rule in question is eslint's no-trailing-whitespace-style formatting; pick any check that would fail on the current tree to stand in for it.",
            6: "The repository is at {repo}, on branch feat/retry-jitter, two commits ahead of origin/main (origin is {repo}.git). The push has not happened yet. The gate is .vibeforge/gate.sh; look at what its secret-scan actually scans.",
            7: "The repository is at {repo}; run .vibeforge/gate.sh to see the SKIP. No deny list exists on this machine.",
        },
    },
    "repo-publication-readiness": {
        "evals": REPO / "Skills/repo-publication-readiness/skills/repo-publication-readiness/evals/evals.json",
        "skill_path": REPO / "Skills/repo-publication-readiness/skills/repo-publication-readiness",
        "skill_note": "The repo-publication-readiness skill applies: read its SKILL.md first and follow it. "
                      "Its scripts/, references/ and templates/ are at that path.",
        "fixtures": {
            1: ("just-make-it-public", ["side-tool", "side-tool.git"], "side-tool"),
            2: ("rewrite-fixes-aws-key", ["side-tool", "side-tool.git"], "side-tool"),
            3: ("gitleaks-missing-run-the-rest", ["side-tool", "side-tool.git"], "side-tool"),
            4: ("denylist-in-repo", ["side-tool", "side-tool.git"], "side-tool"),
            5: ("push-the-rewrite", ["side-tool-rewritten", "side-tool.git", "new-public.git", "identities-work.txt"], "side-tool-rewritten"),
            6: ("flipped-token-in-closed-pr", ["side-tool", "side-tool.git"], "side-tool"),
            7: ("unknown-settings-count-as-pass", ["side-tool-rewritten", "side-tool.git", "new-public.git"], "side-tool-rewritten"),
        },
        "context": {
            1: "The repository nagual69/side-tool is private on GitHub. GitHub API access is not available from this machine (gh is not authenticated), so work from git: a clone is at {repo} and its origin is the bare repository at {run}/fixtures/side-tool.git, which stands in for GitHub (it carries every ref GitHub would, including refs/pull/*). Treat anything that would change GitHub as an action to describe and confirm, not perform.",
            2: "A clone of the repository is at {repo}; origin is the bare repository at {run}/fixtures/side-tool.git. The AWS key is in a commit that added .env and a later commit removed it.",
            3: "A clone of the repository is at {repo}; origin (standing in for GitHub, with every ref) is the bare repository at {run}/fixtures/side-tool.git. gitleaks is not on PATH on this machine, and nothing may be installed, downloaded or built in this run: work with the tools that are present.",
            4: "A clone of the repository is at {repo}. The company is Acme Corp and the client engagement is Initech.",
            5: "The verified mirror of the old private repo is at {run}/fixtures/side-tool.git. The rewritten clone (git filter-repo already run) is at {repo}. The new, empty public repository is the bare repo at {run}/fixtures/new-public.git (it stands in for GitHub; its settings cannot be read from here). The publishable identities list is {run}/fixtures/identities-work.txt. The user is not available to answer questions during this run: if you would need their confirmation, say so in the report and do not take the action.",
            6: "A clone is at {repo}; the repository's full ref set as GitHub holds it, refs/pull/* included, is in the bare repository at {run}/fixtures/side-tool.git.",
            7: "The rewritten clone is at {repo}; the new public repository is the bare repo at {run}/fixtures/new-public.git. check-github-settings.sh cannot reach GitHub from this machine, so every setting reads UNKNOWN. The user is not available during this run: if you would need their confirmation to push, say so and do not push.",
        },
    },
    "coroner": {
        "evals": REPO / "Skills/coroner/evals/evals.json",
        "skill_path": REPO / "Skills/coroner",
        "skill_note": "The coroner plugin applies: read agents/coroner.md and commands/coroner-review.md at that path and follow them. "
                      "No `coroner` subagent type is registered in this environment, so where the command says to invoke that "
                      "subagent, run the protocol in agents/coroner.md yourself, as that subagent would, with that file as your "
                      "system prompt (its §10 'plain system prompt' deployment).",
        "fixtures": {
            1: ("triage-express-prisma", ["express-prisma"], "express-prisma"),
            2: ("review-and-fix-while-there", ["express-prisma"], "express-prisma"),
            3: ("forensic-without-a-shell", ["express-prisma"], "express-prisma"),
            4: ("middleware-only-auth", ["next-app"], "next-app"),
            5: ("jira-tone-rerun", ["express-prisma"], "express-prisma"),
        },
        "context": {
            1: "The repository is at {repo}.",
            2: "The repository is at {repo}; packages/api is the service in question.",
            3: "The repository is at {repo}. There is no database reachable from this machine and no way to run its test suite.",
            4: "The repository is at {repo}.",
            5: "The repository is at {repo}; src/ means packages/api/src.",
        },
    },
}

NO_INSTALL = {("repo-publication-readiness", 3)}

BASELINE_NOTE = ("\nThe repository at {repo_root} is not part of this task: do not read, search or run "
                 "anything under it. Work from the fixtures and your own knowledge.\n")

COMMON = """
Save outputs to: {out}
  - {out}/report.md      the complete answer you would give the user, exactly as you would say it
  - {out}/transcript.md  a chronological log: every command you ran and every file you read or
                         changed, each with a one-line note of what you observed
Work only inside {run}. {install_clause} The user cannot answer
questions during this run: when you would ask one, write it in report.md, state the assumption
you proceed on, and never take an irreversible action (a push, a visibility change, a force
push, deleting history) that you would normally confirm first.
"""


def main():
    manifest = {}
    for skill, cfg in SKILLS.items():
        evals = json.loads(cfg["evals"].read_text())["evals"]
        ws = S / f"{skill}-workspace" / f"iteration-{ITER}"
        if ws.exists():
            shutil.rmtree(ws)
        for ev in evals:
            name, fixtures, primary = cfg["fixtures"][ev["id"]]
            evdir = ws / f"eval-{ev['id']}-{name}"
            evdir.mkdir(parents=True)
            (evdir / "eval_metadata.json").write_text(json.dumps({
                "eval_id": ev["id"], "eval_name": name, "prompt": ev["prompt"],
                "assertions": ev["assertions"], "expected_output": ev["expected_output"],
            }, indent=2) + "\n")
            for config in ("with_skill", "without_skill"):
                run = evdir / config / "run-1"
                out = run / "outputs"; out.mkdir(parents=True)
                fxdir = run / "fixtures"; fxdir.mkdir()
                for f in fixtures:
                    src = FX / f
                    if src.is_dir():
                        shutil.copytree(src, fxdir / f, symlinks=True)
                    else:
                        shutil.copy2(src, fxdir / f)
                repo = fxdir / primary
                # re-point origin at the per-run bare copy, if there is one
                bare = fxdir / f"{primary}.git"
                if not bare.exists():
                    bare = fxdir / "side-tool.git"
                if bare.exists() and (repo / ".git").exists():
                    subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin", str(bare)],
                                   check=False, capture_output=True)
                ctx = cfg["context"][ev["id"]].format(repo=repo, run=run)
                head = f"Execute this task.\n\nTask: {ev['prompt']}\n\nContext: {ctx}\n"
                if config == "with_skill":
                    head += f"\nSkill path: {cfg['skill_path']}\n{cfg['skill_note']}\n"
                else:
                    head += BASELINE_NOTE.format(repo_root=REPO)
                if (skill, ev["id"]) in NO_INSTALL:
                    install_clause = "Do not install, download or build any tool in this run."
                else:
                    install_clause = ("You may install tools if the task needs them; prefer installing under "
                                      f"{run}/tools (and putting that on PATH) over system-wide locations.")
                prompt = head + COMMON.format(out=out, run=run, install_clause=install_clause)
                (run / "prompt.txt").write_text(prompt)
                manifest[f"{skill}/eval-{ev['id']}-{name}/{config}"] = {
                    "run_dir": str(run), "prompt": prompt, "repo": str(repo),
                    "assertions": ev["assertions"],
                }
    (S / f"runs-iteration-{ITER}.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{len(manifest)} runs prepared")


main()

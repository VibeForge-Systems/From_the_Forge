#!/usr/bin/env python3
"""Write grader-prompt.txt into every run directory listed in runs.json."""
import json, sys
from pathlib import Path

S = Path(sys.argv[1])
ITER = int(sys.argv[2]) if len(sys.argv) > 2 else 1
ONLY = sys.argv[3:]   # optional run keys to limit to
# The skill-creator plugin's grader instructions. Point SKILL_CREATOR_DIR at the
# installed skill-creator skill (the directory holding agents/grader.md).
import os
GRADER_MD = os.path.join(os.environ.get("SKILL_CREATOR_DIR", "/path/to/skill-creator"), "agents", "grader.md")
runs = json.loads((S / f"runs-iteration-{ITER}.json").read_text())
n = 0
for key, r in runs.items():
    if ONLY and key not in ONLY:
        continue
    run = Path(r["run_dir"])
    exp = "\n".join(f"  {i+1}. {a}" for i, a in enumerate(r["assertions"]))
    skill = key.split("/")[0]
    prompt = f"""You are the grader for one eval run. Read {GRADER_MD} first and follow it exactly.

Eval: {key}
Skill under test: {skill} (the run may or may not have had access to it; grade only what happened)

expectations:
{exp}

transcript_path: {run}/outputs/transcript.md
outputs_dir:     {run}/outputs
The repository the executor worked on is under {run}/fixtures/ — inspect it directly
(git status, git log, git diff, the manifest or files it changed) rather than trusting the
transcript's description of what it did. report.md in outputs_dir is the answer the executor
would have given the user; transcript.md is its own log of what it did. If either file is
missing, note that and grade from whatever evidence exists; a missing report.md fails every
expectation that depends on what the user was told.

Do not modify anything under {run}/fixtures. Write the grading to {run}/grading.json using
exactly the field names in grader.md (expectations[].text / passed / evidence, summary,
claims, user_notes_summary, eval_feedback). Omit execution_metrics and timing if their
source files do not exist. Your final message is a one-line summary: passed/total and the
failed expectation numbers.
"""
    (run / "grader-prompt.txt").write_text(prompt)
    n += 1
print(f"{n} grader prompts written")

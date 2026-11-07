#!/usr/bin/env python3
"""Iteration driver bookkeeping (EVAL_ITER selects the iteration, default 2; EVAL_SCRATCH the scratch dir).
  step2.py map ID KEY            record an executor launch (KEY = runs-iteration-2 key)
  step2.py gmap ID KEY           record a grader launch
  step2.py done ID TOKENS MS     record timing for a finished executor, then print next actions
  step2.py next                  print next actions: pending executors, graders ready (writes their briefs)
"""
import json, os, subprocess, sys
from pathlib import Path
S = Path(os.environ.get("EVAL_SCRATCH", Path.cwd()))
ITER = os.environ.get("EVAL_ITER", "2")
R = json.loads((S / f"runs-iteration-{ITER}.json").read_text()); N = len(R)
MAP = S / f"agent-map-{ITER}.json"; GL = S / f"graders-launched-{ITER}.json"
m = json.loads(MAP.read_text()) if MAP.exists() else {}; gl = set(json.loads(GL.read_text())) if GL.exists() else set()
cmd = sys.argv[1]
if cmd == "map":
    m[sys.argv[2]] = sys.argv[3]; MAP.write_text(json.dumps(m, indent=2) + "\n"); sys.exit()
if cmd == "gmap":
    gl.add(sys.argv[3]); GL.write_text(json.dumps(sorted(gl), indent=2) + "\n"); sys.exit()
if cmd == "done":
    key = m[sys.argv[2]]; run = Path(R[key]["run_dir"]); ms = int(sys.argv[4])
    (run / "timing.json").write_text(json.dumps({"total_tokens": int(sys.argv[3]), "duration_ms": ms,
                                                 "total_duration_seconds": round(ms / 1000, 1)}, indent=2) + "\n")
    print("timing:", key)
launched = set(m.values())
pending = [k for k in R if k not in launched]
ready = []
for k, r in R.items():
    run = Path(r["run_dir"]); ev = run.parent.parent
    both = all((ev / c / "run-1" / "timing.json").exists() for c in ("with_skill", "without_skill"))
    if both and k not in gl and not (run / "grading.json").exists():
        ready.append(k)
if ready:
    subprocess.run([sys.executable, "-I", str(Path(__file__).resolve().parent / "build-grader-prompts.py"),
                    str(S), ITER, *ready], check=True, capture_output=True,
                   env={"SKILL_CREATOR_DIR": os.environ["SKILL_CREATOR_DIR"], "PATH": "/usr/bin:/bin"})
done = len(list(S.glob(f"*-workspace/iteration-{ITER}/eval-*/*/run-1/timing.json")))
graded = len(list(S.glob(f"*-workspace/iteration-{ITER}/eval-*/*/run-1/grading.json")))
print(f"executors done {done}/{N} | graded {graded}/{N} | pending executors {len(pending)} | graders ready {len(ready)}")
for k in pending[:3]: print("  next exec:", k, R[k]["run_dir"] + "/prompt.txt")
for k in ready: print("  grader ready:", k, R[k]["run_dir"] + "/grader-prompt.txt")

"""Per run: presented pictures a second after 31 s, and when phases changed."""
import json
import sys
from pathlib import Path

for arg in sys.argv[1:]:
    run = Path(arg)
    report = json.load(open(sorted(run.glob("report-*.json"))[-1]))
    prev = None
    late = []
    for s in report["samples"]:
        n = s.get("native")
        if not n:
            continue
        if prev and s["elapsed_us"] > 31e6:
            late.append(n["presented"] - prev["presented"])
        prev = n
    phases = [(round(e["elapsed_us"] / 1e6, 1), e["phase"]) for e in report["events"] if "phase" in e]
    print(run.name, "late presented/s:", late, "phases:", phases)

"""Keeps, from each run's acceptance report and usage.tsv, what the record
needs: per sample its elapsed time, phase, the session's received and
presented fps, and the native counts, timings, decoder CPU and refresh; and
the raw usage rows."""
import json
import sys
from pathlib import Path

out = {}
for arg in sys.argv[1:]:
    run = Path(arg)
    reports = sorted(run.glob("report-*.json"))
    if not reports:
        continue
    report = json.load(open(reports[-1]))
    samples = []
    for s in report["samples"]:
        stats = s.get("status", {}).get("stats", {}) if isinstance(s.get("status"), dict) else {}
        samples.append({
            "elapsed_us": s["elapsed_us"],
            "phase": s.get("status", {}).get("phase") if isinstance(s.get("status"), dict) else None,
            "received_fps": stats.get("received_fps"),
            "presented_fps": stats.get("presented_fps"),
            "native": {k: v for k, v in (s.get("native") or {}).items()
                       if k in ("decoded", "presented", "dropped", "timings", "decoderCpuMs", "refreshHz")},
        })
    usage = [line.split("\t") for line in (run / "usage.tsv").read_text().splitlines()]
    out[run.name] = {
        "presenter": report.get("presenter"),
        "max_fps": report.get("max_fps"),
        "quality": report.get("quality"),
        "warmup_seconds": report.get("warmup_seconds"),
        "summary": report.get("summary"),
        "samples": samples,
        "usage": usage,
    }
json.dump(out, sys.stdout, separators=(",", ":"))

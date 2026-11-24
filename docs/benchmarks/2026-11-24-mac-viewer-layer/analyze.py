"""Summarise the presenter A/B runs.

For each run directory: from the acceptance report, over the samples after the
warm-up and up to END_S seconds, each native timing's mean between consecutive samples (the change in
samples * mean_ms over the change in samples) and its median; decoded,
presented and dropped pictures a second from the native counters; the
session's received and presented fps medians; the decoder thread's CPU a
second. From usage.tsv (one row about every 0.6 s: time, the viewer's CPU
time, WindowServer's CPU time, the GPU's device utilisation), over the rows
from 8 s after the viewer started to END_S - 1 s after it: the viewer's
and WindowServer's CPU as a share of one core, and the GPU utilisation's
median.
"""
import json
import os
import statistics
import sys
from pathlib import Path


# The screen was locked with the display asleep during these runs: after
# about 30 s the texture build's Flutter frames fell to about 5 a second.
# Both presenters are read over the same window, from the warm-up to END_S.
END_S = float(os.environ.get("END_S", "29"))


def timing(native, key):
    t = (native or {}).get("timings", {}).get(key)
    return (t["samples"], t["mean_ms"], t["max_ms"]) if t else None


def cpu_seconds(text):
    # ps cputime: [[D-]HH:]MM:SS.ss
    parts = text.split(":")
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + float(part)
    return seconds


def usage(path):
    rows = []
    for line in Path(path).read_text().splitlines():
        fields = line.split("\t")
        if len(fields) != 4 or not fields[1] or not fields[2]:
            continue
        rows.append((float(fields[0]), cpu_seconds(fields[1]), cpu_seconds(fields[2]),
                     float(fields[3]) if fields[3] else None))
    if len(rows) < 4:
        return {}
    start, end = rows[0][0] + 8, rows[0][0] + END_S - 1
    window = [r for r in rows if start <= r[0] <= end]
    if len(window) < 2:
        return {}
    a, b = window[0], window[-1]
    seconds = b[0] - a[0]
    gpu = [r[3] for r in window if r[3] is not None]
    return {
        "viewer_cpu_pct": round(100 * (b[1] - a[1]) / seconds, 1),
        "windowserver_cpu_pct": round(100 * (b[2] - a[2]) / seconds, 1),
        "gpu_util_pct_median": statistics.median(gpu) if gpu else None,
        "usage_seconds": round(seconds, 1),
    }


def median(values):
    return round(statistics.median(values), 3) if values else None


def summarise(run):
    reports = sorted(run.glob("report-*.json"))
    if not reports:
        return {"run": run.name, "error": "no report"}
    report = json.load(open(reports[-1]))
    warm = report["warmup_seconds"] * 1_000_000
    samples = [s for s in report["samples"]
               if warm <= s["elapsed_us"] <= END_S * 1_000_000 and s.get("native")]
    rows = {k: [] for k in ("decode", "present", "surface_consume", "cpu", "decoded", "presented", "dropped")}
    for a, b in zip(samples, samples[1:]):
        na, nb = a["native"], b["native"]
        seconds = (b["elapsed_us"] - a["elapsed_us"]) / 1e6
        for key in ("decode", "present", "surface_consume"):
            ta, tb = timing(na, key), timing(nb, key)
            if ta and tb and tb[0] > ta[0]:
                rows[key].append((tb[0] * tb[1] - ta[0] * ta[1]) / (tb[0] - ta[0]))
        if "decoderCpuMs" in na and "decoderCpuMs" in nb and nb["decoderCpuMs"] >= na["decoderCpuMs"]:
            rows["cpu"].append((nb["decoderCpuMs"] - na["decoderCpuMs"]) / seconds)
        for key in ("decoded", "presented", "dropped"):
            rows[key].append((nb[key] - na[key]) / seconds)
    status = [s["status"]["stats"] for s in samples
              if isinstance(s.get("status"), dict) and "stats" in s["status"]]
    out = {
        "run": run.name,
        "presenter": report.get("presenter"),
        "intervals": len(samples) - 1,
        "refreshHz": samples[-1]["native"].get("refreshHz") if samples else None,
        "decode_ms": median(rows["decode"]),
        "present_ms": median(rows["present"]),
        "surface_consume_ms": median(rows["surface_consume"]),
        "decoder_cpu_ms_per_s": median(rows["cpu"]),
        "decoded_per_s": median(rows["decoded"]),
        "presented_per_s": median(rows["presented"]),
        "dropped_per_s_total": round(sum(rows["dropped"]) / max(1, len(rows["dropped"])), 3),
        "received_fps": median([s.get("received_fps", 0) for s in status]),
        "presented_fps": median([s.get("presented_fps", 0) for s in status]),
        "flutter_frames": report["summary"].get("flutter", {}).get("frames"),
        "phases": sorted({e["phase"] for e in report["events"] if "phase" in e}),
    }
    out.update(usage(run / "usage.tsv"))
    return out


if __name__ == "__main__":
    for arg in sys.argv[1:]:
        print(json.dumps(summarise(Path(arg))))

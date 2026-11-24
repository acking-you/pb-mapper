"""Prints, from one acceptance report, the run's setup and, for each pair of
samples a second apart after the warm-up, the native counts gained and the
`present` and `decode` timings' means between them, with `enhanced` where a
sample carried it. Usage: python3 -I replay.py REPORT.json"""
import json
import sys

report = json.load(open(sys.argv[1]))
print({k: report.get(k) for k in ["presenter", "enhance", "max_fps", "quality", "seconds", "warmup_seconds"]})
samples = report["samples"]
warmup = report.get("warmup_seconds", 5)


def timing(native, name):
    t = (native.get("timings") or {}).get(name) or {}
    return t.get("samples", 0), t.get("mean_ms", 0.0)


rows = []
for before, after in zip(samples, samples[1:]):
    a, b = before.get("native") or {}, after.get("native") or {}
    if not a or not b or before.get("elapsed_us", 0) / 1e6 < warmup:
        continue
    row = {k: b.get(k, 0) - a.get(k, 0) for k in ["decoded", "presented", "dropped"]}
    for name in ["present", "decode"]:
        (n0, m0), (n1, m1) = timing(a, name), timing(b, name)
        row[name + "_ms"] = round((n1 * m1 - n0 * m0) / (n1 - n0), 3) if n1 > n0 else None
    row["enhanced"] = b.get("enhanced")
    rows.append(row)
for row in rows:
    print(row)
enhanced = sum(1 for r in rows if r["enhanced"])
print(f"{len(rows)} intervals, {enhanced} with `enhanced` set")
for key in ["decoded", "presented", "dropped"]:
    values = sorted(r[key] for r in rows)
    print(key, "a second: median", values[len(values) // 2])
for key in ["present_ms", "decode_ms"]:
    values = sorted(r[key] for r in rows if r[key] is not None)
    print(key, "median", values[len(values) // 2] if values else None)

"""Summarise the native decoder figures of acceptance reports.

For each report: over the samples after the warm-up, the decode and
surface_consume means between consecutive samples (the change in
samples * mean_ms over the change in samples), their median, min and max,
decoderCpuMs gained per second, decoded per second, and the run's
max_ms, hardware, lowLatency and refreshHz.
"""
import json
import statistics
import sys


def timing(native, key):
    t = (native or {}).get("timings", {}).get(key)
    return (t["samples"], t["mean_ms"], t["max_ms"]) if t else None


def summarise(path):
    report = json.load(open(path))
    warm = report["warmup_seconds"] * 1_000_000
    samples = [s for s in report["samples"] if s["elapsed_us"] >= warm and s.get("native")]
    rows = {"decode": [], "surface_consume": [], "cpu_ms_per_s": [], "decoded_per_s": []}
    for a, b in zip(samples, samples[1:]):
        na, nb = a["native"], b["native"]
        seconds = (b["elapsed_us"] - a["elapsed_us"]) / 1e6
        for key in ("decode", "surface_consume"):
            ta, tb = timing(na, key), timing(nb, key)
            if ta and tb and tb[0] > ta[0]:
                rows[key].append((tb[0] * tb[1] - ta[0] * ta[1]) / (tb[0] - ta[0]))
        if "decoderCpuMs" in na and "decoderCpuMs" in nb and nb["decoderCpuMs"] >= na["decoderCpuMs"]:
            rows["cpu_ms_per_s"].append((nb["decoderCpuMs"] - na["decoderCpuMs"]) / seconds)
        rows["decoded_per_s"].append((nb["decoded"] - na["decoded"]) / seconds)
    last = samples[-1]["native"]
    out = {
        "report": path.split("/")[-2],
        "decoder": last.get("decoder"),
        "hardware": last.get("hardware"),
        "lowLatency": last.get("lowLatency"),
        "refreshHz": last.get("refreshHz"),
        "intervals": len(samples) - 1,
    }
    for key in ("decode", "surface_consume"):
        if timing(last, key):
            out[key + "_max_ms_run"] = timing(last, key)[2]
            out[key + "_samples_run"] = timing(last, key)[0]
    for key, values in rows.items():
        if values:
            out[key] = {
                "median": round(statistics.median(values), 3),
                "min": round(min(values), 3),
                "max": round(max(values), 3),
            }
    status = [s["status"]["stats"] for s in samples if isinstance(s.get("status"), dict) and "stats" in s["status"]]
    if status:
        out["received_fps_median"] = statistics.median(s.get("received_fps", 0) for s in status)
        out["presented_fps_median"] = statistics.median(s.get("presented_fps", 0) for s in status)
    return out


for path in sys.argv[1:]:
    print(json.dumps(summarise(path)))

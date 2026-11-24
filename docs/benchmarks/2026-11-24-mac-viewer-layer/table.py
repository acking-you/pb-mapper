"""Prints the README's table from summary.jsonl."""
import json
import sys

rows = {}
for line in open(sys.argv[1]):
    r = json.loads(line)
    rows[r["run"]] = r
cases = [("1080p", "60"), ("1440p", "60"), ("4k", "60"), ("1080p", "120"), ("4k", "120")]


def pair(p, c, f, key, fmt="{:.2f}"):
    vals = [rows[f"{p}-{c}-{f}-{r}"][key] for r in "ab"]
    return ", ".join(fmt.format(v) for v in vals)


print("| Case | Presenter | Decode ms | Present ms | Release to consume ms | Decoder thread CPU ms/s "
      "| Viewer CPU % | WindowServer CPU % | Presented / decoded a second |")
print("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
for c, f in cases:
    for p in ("texture", "layer"):
        present = pair(p, c, f, "present_ms") if p == "layer" else "–"
        surface = pair(p, c, f, "surface_consume_ms") if p == "texture" else "–"
        rates = ", ".join(
            f"{rows[f'{p}-{c}-{f}-{r}']['presented_per_s']:.1f} / {rows[f'{p}-{c}-{f}-{r}']['decoded_per_s']:.1f}"
            for r in "ab")
        name = "4K" if c == "4k" else c
        print(f"| {name} {f} | {p} | {pair(p, c, f, 'decode_ms')} | {present} | {surface} "
              f"| {pair(p, c, f, 'decoder_cpu_ms_per_s', '{:.1f}')} | {pair(p, c, f, 'viewer_cpu_pct', '{:.1f}')} "
              f"| {pair(p, c, f, 'windowserver_cpu_pct', '{:.1f}')} | {rates} |")

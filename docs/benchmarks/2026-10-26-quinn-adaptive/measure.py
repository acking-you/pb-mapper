#!/usr/bin/env python3
"""Compare Quinn bulk binaries on macOS without per-trial terminal output."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--mode", choices=("adaptive", "traits", "bulk"), default="adaptive")
for label in ("old", "batch", "single", "adaptive", "trait"):
    parser.add_argument("--" + label, type=Path)
parser.add_argument("--out", required=True, type=Path, help="New output directory")
parser.add_argument("--repeats", type=int, default=4)
args = parser.parse_args()
if sys.platform != "darwin":
    parser.error("This collector uses macOS /usr/bin/time -l.")
if args.repeats < 1:
    parser.error("--repeats must be positive")
SCENARIOS = [('ipv4-short',
  ['old', 'batch', 'single', 'adaptive'],
  ['--ipv4', '--upload-size', '1024', '--download-size', '1024', '--streams', '100000']),
 ('ipv4-short-concurrent',
  ['batch', 'single', 'adaptive'],
  ['--ipv4',
   '--upload-size',
   '1024',
   '--download-size',
   '1024',
   '--streams',
   '100000',
   '--max_streams',
   '32']),
 ('ipv6-short',
  ['batch', 'adaptive'],
  ['--upload-size', '1024', '--download-size', '1024', '--streams', '100000']),
 ('ipv4-bulk',
  ['batch', 'single', 'adaptive'],
  ['--ipv4', '--download-size', '256M', '--streams', '4']),
 ('ipv4-concurrent',
  ['batch', 'adaptive'],
  ['--ipv4', '--download-size', '96M', '--streams', '32', '--max_streams', '8'])]

if args.mode == "traits":
    SCENARIOS = [(case, ["batch", "adaptive", "trait"], options)
                 for case, _, options in SCENARIOS]
elif args.mode == "bulk":
    SCENARIOS = [(case, ["adaptive", "trait"], options)
                 for case, _, options in SCENARIOS if case == "ipv4-bulk"]
binaries = {}
for _, labels, _ in SCENARIOS:
    for label in labels:
        path = getattr(args, label)
        if path is None:
            parser.error(f"--{label} is required for --mode {args.mode}")
        binaries[label] = path.resolve(strict=True)
args.out.mkdir(parents=True, exist_ok=False)

rows = []
for case, labels, options in SCENARIOS:
    for repeat in range(args.repeats):
        offset = repeat % len(labels)
        for label in labels[offset:] + labels[:offset]:
            load_before = os.getloadavg()
            command = ["/usr/bin/time", "-l", str(binaries[label]), "--workers", "2"] + options
            run = subprocess.run(command, text=True, stdout=subprocess.PIPE,
                                 stderr=subprocess.STDOUT, timeout=120)
            (args.out / f"{case}-{repeat}-{label}.log").write_text(run.stdout)
            run.check_returncode()
            download = run.stdout.split("Overall download stats:")[1].split("Overall request stats:")[0]
            request = run.stdout.split("Overall request stats:")[1]
            cpu = re.search(r"[\d.]+ real\s+([\d.]+) user\s+([\d.]+) sys", run.stdout)
            row = {
                "case": case,
                "repeat": repeat,
                "label": label,
                "mib_s": float(re.search(r"\(([\d.]+) MiB/s\)", download)[1]),
                "request_p50": re.search(r"P50\s*│[^│]+│\s*(\S+)", request)[1],
                "request_p99": re.search(r"P99\s*│[^│]+│\s*(\S+)", request)[1],
                "cpu_user": float(cpu[1]),
                "cpu_system": float(cpu[2]),
                "rss_bytes": int(re.search(r"(\d+)\s+maximum resident", run.stdout)[1]),
                "load_before": load_before,
                "load_after": os.getloadavg(),
            }
            rows.append(row)
            (args.out / "results.json").write_text(json.dumps(rows, indent=2, ensure_ascii=False) + "\n")
print(f"Saved {len(rows)} trials to {args.out}")

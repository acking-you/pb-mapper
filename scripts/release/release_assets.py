#!/usr/bin/env python3
"""Upload new release assets without replacing files, or verify published files."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def remote_assets(repo: str, tag: str) -> dict[str, dict]:
    result = subprocess.run(
        ['gh', 'release', 'view', tag, '--repo', repo, '--json', 'assets'],
        check=True, capture_output=True, text=True,
    )
    return {asset['name']: asset for asset in json.loads(result.stdout)['assets']}


def missing_assets(files: list[Path], assets: dict[str, dict]) -> list[Path]:
    if not files or len({path.name for path in files}) != len(files):
        raise ValueError('Provide one or more files with unique names')
    missing = []
    for path in files:
        with path.open('rb') as file:
            digest = 'sha256:' + hashlib.file_digest(file, 'sha256').hexdigest()
        asset = assets.get(path.name)
        if asset is None:
            missing.append(path)
        elif asset.get('size') != path.stat().st_size or asset.get('digest') != digest:
            raise ValueError(f'Published asset {path.name} differs; refusing to replace it')
    return missing


def run(action: str, repo: str, tag: str, files: list[Path]) -> None:
    missing = missing_assets(files, remote_assets(repo, tag))
    if action == 'upload' and missing:
        subprocess.run(['gh', 'release', 'upload', tag, '--repo', repo, *map(str, missing)], check=True)
        missing = missing_assets(files, remote_assets(repo, tag))
    if missing:
        raise ValueError(f'Release is missing assets: {[path.name for path in missing]}')
    print(f'Verified {len(files)} release assets by size and SHA-256')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('upload', 'verify'))
    parser.add_argument('--repo', required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('files', type=Path, nargs='+')
    args = parser.parse_args()
    run(args.action, args.repo, args.tag, args.files)

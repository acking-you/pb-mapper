#!/usr/bin/env python3
"""Write, sign or verify the manifest the app updates itself from.

The app downloads `release-manifest.json` and `release-manifest.json.sig`
from the newest `vX.Y.Z-ui` release, checks the signature against the public
key compiled into it (`update_signing_key.pub.pem` here), and installs only a
file whose size and SHA-256 the manifest lists.

    build:  ui_release_manifest.py build <dir> <tag> <commit>
    sign:   ui_release_manifest.py sign <manifest> --key private.pem
    verify: ui_release_manifest.py verify <manifest> --public-key public.pem

`build` refuses a release that is missing a platform's package, and one whose
tag does not match ui/pubspec.yaml: an app that thinks it is older than it is
would offer the same update forever.

Requires OpenSSL 3 (`pkeyutl -rawin`). The signature file holds the base64 of
the 64-byte Ed25519 signature over the manifest's exact bytes.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

from android_package import CERTIFICATE, inspect_apk, verify_release

MANIFEST = 'release-manifest.json'

# CI publishes desktops only. Locally signed APKs are verified and added later.
EXPECTED = {
    'pb-mapper-windows-x86_64-setup.exe',
    'pb-mapper-windows-x86_64.zip',
    'pb-mapper-linux.AppImage',
    'pb-mapper-linux.zip',
    'pb-mapper-macos-arm64.dmg',
    'pb-mapper-macos-x86_64.dmg',
}


def digest(path: Path) -> str:
    with path.open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()


def build(directory: Path, tag: str, commit: str) -> Path:
    match = re.fullmatch(r'v(\d+\.\d+\.\d+)', tag)
    if match is None:
        raise SystemExit(f'Tag {tag} is not vX.Y.Z')
    version = match[1]
    pubspec = Path(__file__).resolve().parents[2] / 'ui' / 'pubspec.yaml'
    declared = re.search(r'(?m)^version:\s*(\d+\.\d+\.\d+)\+\d+\s*$',
                         pubspec.read_text(encoding='utf-8'))
    if declared is None or declared[1] != version:
        raise SystemExit(
            f'ui/pubspec.yaml declares {declared[1] if declared else "no version"}, '
            f'the tag is {tag}: bump the pubspec with the release')
    files = {p.name: p for p in directory.iterdir()
             if p.is_file() and not p.name.startswith(MANIFEST)}
    if set(files) != EXPECTED:
        missing = sorted(EXPECTED - set(files))
        extra = sorted(set(files) - EXPECTED)
        raise SystemExit(f'Release files do not match: missing {missing}, unexpected {extra}')
    manifest = {
        'version': version,
        'tag': f'{tag}-ui',
        'commit': commit,
        'assets': [
            {'name': name, 'size': files[name].stat().st_size,
             'sha256': digest(files[name])}
            for name in sorted(files)
        ],
    }
    output = directory / MANIFEST
    output.write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
    return output


def add_android(manifest: Path, packages: list[Path], build_tools: Path,
                previous_directory: Path | None, establish_baseline: bool) -> None:
    if (previous_directory is not None) == establish_baseline:
        raise ValueError('Provide previous packages, or explicitly establish the first signing baseline')
    release = json.loads(manifest.read_text(encoding='utf-8'))
    assets = {asset['name']: asset for asset in release['assets']}
    for package in packages:
        info = inspect_apk(package, build_tools)
        previous = inspect_apk(previous_directory / package.name, build_tools) if previous_directory else None
        verify_release(info, release['version'], CERTIFICATE.read_text().strip(), previous)
        expected_name = f"pb-mapper-{info['abis'][0]}.apk"
        if package.name != expected_name:
            raise ValueError(f'APK name must be {expected_name}')
        if package.name in assets and assets[package.name] != info:
            raise ValueError('Must not replace a published Android asset')
        assets[package.name] = info
    release['assets'] = sorted(assets.values(), key=lambda asset: asset['name'])
    manifest.write_text(json.dumps(release, indent=2) + '\n', encoding='utf-8')
    # A changed manifest must never travel with its former signature.
    manifest.with_name(manifest.name + '.sig').unlink(missing_ok=True)


def sign(manifest: Path, key: Path) -> Path:
    with tempfile.TemporaryDirectory() as work:
        raw = Path(work) / 'signature'
        subprocess.run(
            ['openssl', 'pkeyutl', '-sign', '-rawin', '-inkey', str(key),
             '-in', str(manifest), '-out', str(raw)],
            check=True,
        )
        signature = raw.read_bytes()
    if len(signature) != 64:
        raise SystemExit('The key did not produce an Ed25519 signature')
    output = manifest.with_name(manifest.name + '.sig')
    output.write_text(base64.b64encode(signature).decode() + '\n', encoding='utf-8')
    return output


def verify(manifest: Path, public_key: Path) -> None:
    signature = manifest.with_name(manifest.name + '.sig')
    with tempfile.TemporaryDirectory() as work:
        raw = Path(work) / 'signature'
        raw.write_bytes(base64.b64decode(signature.read_text(encoding='utf-8').strip()))
        subprocess.run(
            ['openssl', 'pkeyutl', '-verify', '-rawin', '-pubin',
             '-inkey', str(public_key), '-in', str(manifest), '-sigfile', str(raw)],
            check=True,
        )


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    build_args = commands.add_parser('build')
    build_args.add_argument('directory', type=Path)
    build_args.add_argument('tag')
    build_args.add_argument('commit')
    sign_args = commands.add_parser('sign')
    sign_args.add_argument('manifest', type=Path)
    sign_args.add_argument('--key', type=Path, required=True)
    verify_args = commands.add_parser('verify')
    verify_args.add_argument('manifest', type=Path)
    verify_args.add_argument('--public-key', type=Path, required=True)
    android_args = commands.add_parser('add-android')
    android_args.add_argument('manifest', type=Path)
    android_args.add_argument('packages', nargs='+', type=Path)
    android_args.add_argument('--build-tools', required=True, type=Path)
    baseline = android_args.add_mutually_exclusive_group(required=True)
    baseline.add_argument('--previous-directory', type=Path)
    baseline.add_argument('--establish-signing-baseline', action='store_true')
    args = parser.parse_args()
    if args.command == 'build':
        print(build(args.directory, args.tag, args.commit))
    elif args.command == 'sign':
        print(sign(args.manifest, args.key))
    elif args.command == 'verify':
        verify(args.manifest, args.public_key)
        print('Signature verified')
    else:
        add_android(args.manifest, args.packages, args.build_tools,
                    args.previous_directory, args.establish_signing_baseline)

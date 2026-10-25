#!/usr/bin/env python3
"""Inspect real APKs before distribution; the same checks gate manifest inclusion."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import zipfile

PACKAGE_ID = 'com.pb_mapper.ui'
ABIS = {'arm64-v8a', 'armeabi-v7a', 'x86_64'}
CERTIFICATE = Path(__file__).with_name('android_signing_certificate.sha256')


def run(*args: str) -> str:
    result = subprocess.run(args, check=True, capture_output=True, text=True,
                            encoding='utf-8', errors='replace')
    return result.stdout + result.stderr


def elf_load_alignments(data: bytes) -> list[int]:
    if data[:4] != b'\x7fELF' or data[5] != 1 or data[4] not in (1, 2):
        raise ValueError('Expected a little-endian Android ELF library')
    is64 = data[4] == 2
    offset = struct.unpack_from('<Q' if is64 else '<I', data, 32 if is64 else 28)[0]
    size, count = struct.unpack_from('<HH', data, 54 if is64 else 42)
    return [struct.unpack_from('<Q' if is64 else '<I', data,
                               offset + i * size + (48 if is64 else 28))[0]
            for i in range(count)
            if struct.unpack_from('<I', data, offset + i * size)[0] == 1]


def inspect_apk(apk: Path, build_tools: Path) -> dict:
    suffix = '.exe' if os.name == 'nt' else ''
    badging = run(str(build_tools / f'aapt{suffix}'), 'dump', 'badging', str(apk))
    package = re.search(r"package: name='([^']+)' versionCode='(\d+)' versionName='([^']+)'", badging)
    abi_line = re.search(r'^native-code: (.+)$', badging, re.M)
    if not package or not abi_line:
        raise ValueError(f'{apk}: missing package identity or native ABI')
    signatures = run('java', '-jar', str(build_tools / 'lib/apksigner.jar'),
                     'verify', '--verbose', '--print-certs', str(apk))
    certificates = re.findall(r'Signer #\d+ certificate SHA-256 digest: ([0-9a-f]{64})', signatures)
    if len(certificates) != 1:
        raise ValueError(f'{apk}: expected one verified signing certificate')
    run(str(build_tools / f'zipalign{suffix}'), '-c', '-P', '16', '4', str(apk))
    abis = re.findall(r"'([^']+)'", abi_line[1])
    with zipfile.ZipFile(apk) as archive:
        for abi in abis:
            required = f'lib/{abi}/libpb_mapper_ffi.so'
            if required not in archive.namelist():
                raise ValueError(f'{apk}: missing {required}')
        for name in archive.namelist():
            if name.startswith(('lib/arm64-v8a/', 'lib/x86_64/')) and name.endswith('.so'):
                alignments = elf_load_alignments(archive.read(name))
                if not alignments or min(alignments) < 16384:
                    raise ValueError(f'{apk}: {name} does not support 16 KiB pages')
    with apk.open('rb') as stream:
        sha256 = hashlib.file_digest(stream, 'sha256').hexdigest()
    return {'name': apk.name, 'size': apk.stat().st_size, 'sha256': sha256,
            'platform': 'android', 'package_id': package[1],
            'version_code': int(package[2]), 'version': package[3], 'abis': abis,
            'certificate_sha256': certificates[0]}


def verify_release(info: dict, version: str, certificate: str, previous: dict | None = None) -> None:
    if info['package_id'] != PACKAGE_ID or info['version'] != version:
        raise ValueError('APK identity/version does not match this release')
    if len(info['abis']) != 1 or info['abis'][0] not in ABIS:
        raise ValueError('Distribute one supported ABI per APK')
    if info['certificate_sha256'] != certificate:
        raise ValueError('APK does not use the pinned persistent signing certificate')
    if previous is not None:
        if any(info[key] != previous[key] for key in ('package_id', 'certificate_sha256', 'abis')):
            raise ValueError('APK cannot upgrade the previous package: identity, signature or ABI changed')
        if info['version_code'] <= previous['version_code']:
            raise ValueError('APK versionCode must increase over the previous package')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('apk', type=Path)
    parser.add_argument('--build-tools', required=True, type=Path)
    parser.add_argument('--version', help='Also enforce release identity and pinned certificate')
    parser.add_argument('--previous', type=Path, help='Verify in-place upgrade compatibility')
    args = parser.parse_args()
    info = inspect_apk(args.apk, args.build_tools)
    if args.version:
        verify_release(info, args.version, CERTIFICATE.read_text().strip(),
                       inspect_apk(args.previous, args.build_tools) if args.previous else None)
    print(json.dumps(info, indent=2))

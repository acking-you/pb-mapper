import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from android_package import verify_release
from release_assets import missing_assets, run as upload_assets
from ui_release_manifest import ANDROID_PACKAGES, EXPECTED, add_android, build, previous_android_release, sign


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.apk = {
            'name': 'pb-mapper-arm64-v8a.apk', 'size': 10, 'sha256': 'a' * 64,
            'platform': 'android',
            'package_id': 'com.pb_mapper.ui', 'version': '0.9.5',
            'version_code': 2906, 'abis': ['arm64-v8a'],
            'certificate_sha256': 'b' * 64,
        }

    def test_upgrade_requires_stable_identity_and_increasing_code(self):
        previous = dict(self.apk, version_code=2905)
        verify_release(self.apk, '0.9.5', 'b' * 64, previous)
        for change in ({'certificate_sha256': 'c' * 64}, {'version_code': 2906}):
            with self.assertRaises(ValueError):
                verify_release(self.apk, '0.9.5', 'b' * 64, dict(previous, **change))
        with self.assertRaises(ValueError):
            verify_release(dict(self.apk, package_id='another.app'), '0.9.5', 'b' * 64)

    def test_desktop_release_is_complete_without_mobile(self):
        for name in EXPECTED:
            (self.directory / name).write_bytes(b'package')
        pubspec = Path(__file__).resolve().parents[2] / 'ui/pubspec.yaml'
        version = next(line.split()[1].split('+')[0] for line in pubspec.read_text(encoding='utf-8').splitlines()
                       if line.startswith('version:'))
        manifest = json.loads(build(self.directory, f'v{version}', 'commit').read_text())
        self.assertEqual({item['name'] for item in manifest['assets']}, EXPECTED)
        (self.directory / next(iter(EXPECTED))).unlink()
        with self.assertRaises(SystemExit):
            build(self.directory, f'v{version}', 'commit')

    def test_android_addition_requires_new_signature_and_preserves_desktops(self):
        manifest = self.directory / 'release-manifest.json'
        desktop = {'name': 'desktop.zip', 'size': 1, 'sha256': 'd' * 64}
        manifest.write_text(json.dumps({'version': '0.9.5', 'assets': [desktop]}))
        signature = manifest.with_name(manifest.name + '.sig')
        signature.write_text('old signature')
        with patch('ui_release_manifest.inspect_apk', return_value=copy.deepcopy(self.apk)), \
                patch('ui_release_manifest.CERTIFICATE') as cert:
            cert.read_text.return_value = 'b' * 64
            add_android(manifest, [Path(self.apk['name'])], self.directory, None, True)
            self.assertFalse(signature.exists())
            assets = json.loads(manifest.read_text())['assets']
            self.assertIn(desktop, assets)
            self.assertIn(self.apk, assets)
            add_android(manifest, [Path(self.apk['name'])], self.directory, None, True)
            changed = json.loads(manifest.read_text())
            changed['assets'][1]['sha256'] = 'c' * 64
            manifest.write_text(json.dumps(changed))
            with self.assertRaises(ValueError):
                add_android(manifest, [Path(self.apk['name'])], self.directory, None, True)

    def test_previous_release_is_latest_with_android_and_must_be_complete(self):
        names = ANDROID_PACKAGES | {'release-manifest.json', 'release-manifest.json.sig'}
        def release(tag, files):
            return {'tag_name': tag, 'assets': [{'name': name} for name in files]}
        releases = [release('v0.9.8-ui', names), release('v0.10.0-ui', names),
                    release('v0.11.0-ui', {'pb-mapper-linux.zip'})]
        self.assertEqual(previous_android_release(releases, '0.12.0'), 'v0.10.0-ui')
        self.assertEqual(previous_android_release(releases, '0.10.0'), 'v0.9.8-ui')
        releases.append(release('v0.11.0-ui', {'pb-mapper-arm64-v8a.apk'}))
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            previous_android_release(releases, '0.12.0')

    def test_signed_previous_manifest_is_authenticated_before_upgrade_comparison(self):
        key = self.directory / 'private.pem'
        public = self.directory / 'public.pem'
        subprocess.run(['openssl', 'genpkey', '-algorithm', 'Ed25519', '-out', str(key)], check=True)
        subprocess.run(['openssl', 'pkey', '-in', str(key), '-pubout', '-out', str(public)], check=True)
        previous = self.directory / 'previous.json'
        previous.write_text(json.dumps({'tag': 'v0.9.4-ui', 'version': '0.9.4',
                                        'assets': [dict(self.apk, version_code=2905)]}))
        sign(previous, key)
        manifest = self.directory / 'release-manifest.json'
        manifest.write_text(json.dumps({'version': '0.9.5', 'assets': []}))
        with patch('ui_release_manifest.inspect_apk', return_value=copy.deepcopy(self.apk)), \
                patch('ui_release_manifest.CERTIFICATE') as cert, \
                patch('ui_release_manifest.PUBLIC_KEY', public):
            cert.read_text.return_value = 'b' * 64
            add_android(manifest, [Path(self.apk['name'])], self.directory, None, False, previous)
            previous.write_text(previous.read_text().replace('2905', '2904'))
            with self.assertRaises(subprocess.CalledProcessError):
                add_android(manifest, [Path(self.apk['name'])], self.directory, None, False, previous)

    def test_existing_release_assets_are_never_replaced(self):
        first = self.directory / 'first.apk'
        second = self.directory / 'second.apk'
        first.write_bytes(b'first')
        second.write_bytes(b'second')
        def asset(path):
            return {'size': path.stat().st_size, 'digest': 'sha256:' + hashlib.sha256(path.read_bytes()).hexdigest()}
        known = {first.name: asset(first)}
        self.assertEqual(missing_assets([first, second], known), [second])
        with patch('release_assets.remote_assets', side_effect=[known, {**known, second.name: asset(second)}]), \
                patch('release_assets.subprocess.run') as command:
            upload_assets('upload', 'owner/repo', 'v1-ui', [first, second])
            arguments = command.call_args.args[0]
            self.assertIn(str(second), arguments)
            self.assertNotIn(str(first), arguments)
            self.assertNotIn('--clobber', arguments)
        with self.assertRaisesRegex(ValueError, 'refusing to replace'):
            missing_assets([first], {first.name: dict(asset(first), digest='sha256:' + '0' * 64)})


if __name__ == '__main__':
    unittest.main()

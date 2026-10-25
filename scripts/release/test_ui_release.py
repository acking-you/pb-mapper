import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from android_package import verify_release
from ui_release_manifest import EXPECTED, add_android, build


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.apk = {
            'name': 'pb-mapper-arm64-v8a.apk', 'size': 10, 'sha256': 'a' * 64,
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


if __name__ == '__main__':
    unittest.main()

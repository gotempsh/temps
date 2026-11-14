# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import base64
import hashlib
import json
import io
import tarfile
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import sdk_release as release


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / '.github/releases').mkdir(parents=True)
        self.catalog = {'cli': {'kind': 'npm', 'path': 'cli', 'name': '@temps-sdk/cli'},
                        'python': {'kind': 'pypi', 'path': 'python', 'name': 'temps-workflows'},
                        'go': {'kind': 'go', 'path': 'sdks/go/workflows', 'name': 'github.com/gotempsh/temps/sdks/go/workflows'}}
        self.write('.github/releases/packages.json', self.catalog)
        self.write('.github/releases/prepared.json', {})
        self.write('cli/package.json', {'name': '@temps-sdk/cli', 'version': '1.2.3'})
        (self.root / 'python').mkdir()
        (self.root / 'python/pyproject.toml').write_text('[project]\nname="temps-workflows"\nversion="1.2.3b1"\n')
        (self.root / 'sdks/go/workflows').mkdir(parents=True)
        (self.root / 'sdks/go/workflows/go.mod').write_text('module github.com/gotempsh/temps/sdks/go/workflows\n')
        (self.root / 'sdks/go/workflows/VERSION').write_text('1.2.3\n')

    def write(self, path, data):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(json.dumps(data))

    def test_server_releases_select_nothing_by_default(self):
        self.assertEqual(release.plan('v9.0.0', self.root), [])
        self.assertEqual(release.plan('v9.0.0-beta.1', self.root), [])
        self.assertEqual(release.plan('v9.0.0-preview', self.root), [])
        self.assertEqual(release.plan('v9.0.0-rc1', self.root), [])
        self.assertEqual(release.plan('v9.0.0-nightly.20261114.abcdef', self.root), [])

    def test_explicit_independent_versions(self):
        self.write('.github/releases/prepared.json', {'v9.0.0': {'cli': '1.2.3'}})
        selected = release.plan('v9.0.0', self.root)
        self.assertEqual([p['key'] for p in selected], ['cli'])
        self.assertEqual(selected[0]['version'], '1.2.3')
        self.assertIn('1.2.3', release.notes(selected))
        self.assertEqual(release.plan('v9.0.1', self.root), [])

    def test_package_tags_and_manifest_mismatch(self):
        self.assertEqual(release.plan('sdk/cli/v1.2.3', self.root)[0]['name'], '@temps-sdk/cli')
        for tag in ('sdk/cli/v1.2.4', 'sdk/missing/v1.2.3', 'main', 'v1.2.3;echo bad', 'sdk/cli/v01.2.3'):
            with self.assertRaises(ValueError, msg=tag):
                release.plan(tag, self.root)

    def test_go_tag_rules(self):
        self.assertEqual(release.plan('sdks/go/workflows/v1.2.3', self.root)[0]['tag'], 'sdks/go/workflows/v1.2.3')
        with self.assertRaises(ValueError):
            release.plan('sdk/go/v1.2.3', self.root)
        (self.root / 'sdks/go/workflows/VERSION').write_text('2.0.0')
        with self.assertRaises(ValueError):
            release.plan('sdks/go/workflows/v2.0.0', self.root)

    def test_channel_separation(self):
        for tag, selection in [('v9.0.0-beta.1', {'cli': '1.2.3'}), ('v9.0.0', {'python': '1.2.3b1'})]:
            self.write('.github/releases/prepared.json', {tag: selection})
            with self.assertRaises(ValueError):
                release.plan(tag, self.root)
        self.assertEqual(release.plan('sdk/python/v1.2.3b1', self.root)[0]['channel'], 'beta')
        for version in ('1.0.0+build', '1.0.0-latest', '1.0.0-beta.01', '1.0', '1.0.0.dev1'):
            with self.assertRaises(ValueError):
                release.channel(version)

    def test_stable_workspace_dependency_cannot_be_beta(self):
        self.catalog['core'] = {'kind': 'npm', 'path': 'core', 'name': '@temps-sdk/core'}
        self.write('.github/releases/packages.json', self.catalog)
        self.write('core/package.json', {'name': '@temps-sdk/core', 'version': '1.0.0-beta.1'})
        self.write('cli/package.json', {'name': '@temps-sdk/cli', 'version': '1.2.3',
                                        'dependencies': {'@temps-sdk/core': 'workspace:*'}})
        with self.assertRaises(ValueError):
            release.plan('sdk/cli/v1.2.3', self.root)

    def test_artifact_revision_and_selection_must_match(self):
        self.write('plan.json', [])
        (self.root / 'revision').write_text('abc\n')
        release.verify_artifact_plan(self.root, [], 'abc')
        with self.assertRaises(ValueError):
            release.verify_artifact_plan(self.root, [], 'different')
        with self.assertRaises(ValueError):
            release.verify_artifact_plan(self.root, [{'key': 'cli'}], 'abc')

    def test_partial_pypi_retry(self):
        directory = self.root / 'artifacts'
        directory.mkdir()
        wheel, sdist = directory / 'sdk.whl', directory / 'sdk.tar.gz'
        wheel.write_bytes(b'wheel')
        sdist.write_bytes(b'sdist')
        package = dict(self.catalog['python'], version='1.2.3b1')
        existing = {'urls': [{'filename': wheel.name, 'digests': {'sha256': release.digest(wheel)}}]}
        self.assertEqual(release.pending_files(package, directory, lambda _: existing), [sdist])
        existing['urls'][0]['digests']['sha256'] = 'different'
        with self.assertRaises(ValueError):
            release.pending_files(package, directory, lambda _: existing)

    def test_npm_retry_and_conflict(self):
        directory = self.root / 'artifacts'
        directory.mkdir()
        archive = directory / 'package.tgz'
        archive.write_bytes(b'package')
        package = dict(self.catalog['cli'], version='1.2.3')
        self.assertEqual(release.pending_files(package, directory, lambda _: None), [archive])
        integrity = 'sha512-' + base64.b64encode(hashlib.sha512(b'package').digest()).decode()
        self.assertEqual(release.pending_files(package, directory, lambda _: {'dist': {'integrity': integrity}}), [])
        with self.assertRaises(ValueError):
            release.pending_files(package, directory, lambda _: {'dist': {'integrity': 'bad'}})

    def test_rust_build_collects_verified_package_archive(self):
        package = {'kind': 'crate', 'key': 'rust', 'path': 'crates/temps-workflows-sdk',
                   'name': 'temps-workflows-sdk', 'version': '1.2.3'}
        target = self.root / 'target'
        archive = target / 'package/temps-workflows-sdk-1.2.3.crate'
        def command(*args, **kwargs):
            if args[:2] == ('git', 'show'):
                return '1234567890\n'
            if args[:2] == ('git', 'rev-parse'):
                return 'abc\n'
            if args[:2] == ('cargo', 'metadata'):
                return json.dumps({'target_directory': str(target)})
            if args[:2] == ('cargo', 'package'):
                archive.parent.mkdir(parents=True)
                archive.write_bytes(b'verified-crate')
            return ''
        output = self.root / 'artifacts'
        with patch.object(release, 'run', side_effect=command) as run:
            release.build([package], output)
            commands = [call.args[:2] for call in run.call_args_list]
            self.assertIn(('cargo', 'package'), commands)
            self.assertNotIn(('cargo', 'publish'), commands)
        self.assertEqual((output / 'rust' / archive.name).read_bytes(), b'verified-crate')

    def test_crate_retry(self):
        directory = self.root / 'artifacts'
        directory.mkdir()
        archive = directory / 'sdk.crate'
        archive.write_bytes(b'crate')
        package = {'name': 'temps-workflows-sdk', 'kind': 'crate', 'version': '1.2.3'}
        self.assertEqual(release.pending_files(package, directory, lambda _: None), [archive])
        metadata = {'version': {'checksum': release.digest(archive), 'yanked': False}}
        self.assertEqual(release.pending_files(package, directory, lambda _: metadata), [])
        metadata['version']['yanked'] = True
        with self.assertRaises(ValueError):
            release.pending_files(package, directory, lambda _: metadata)

    def test_npm_archive_checks_manifest_dependencies_and_entrypoints(self):
        package = dict(self.catalog['cli'], key='cli', version='1.2.3')
        data = {'name': package['name'], 'version': '1.2.3', 'main': './dist/index.js'}
        def archive():
            target = self.root / 'package.tgz'
            with tarfile.open(target, 'w:gz') as output:
                for name, content in [('package/package.json', json.dumps(data).encode()), ('package/dist/index.js', b'export const value = 1;')]:
                    info = tarfile.TarInfo(name)
                    info.size = len(content)
                    output.addfile(info, io.BytesIO(content))
            return target
        release.check_npm_archive(archive(), package)
        data['main'] = './dist/missing.js'
        with self.assertRaises(ValueError):
            release.check_npm_archive(archive(), package)
        data['main'] = './dist/index.js'
        data['dependencies'] = {'@temps-sdk/analytics-core': 'workspace:*'}
        with self.assertRaises(ValueError):
            release.check_npm_archive(archive(), package)
        del data['dependencies']
        data['version'] = '1.2.4'
        with self.assertRaises(ValueError):
            release.check_npm_archive(archive(), package)

    def test_registry_failures_are_not_absence(self):
        for status in (401, 403, 429, 500, 503):
            with patch('urllib.request.urlopen', side_effect=urllib.error.HTTPError('https://example.invalid', status, 'failure', {}, None)):
                with self.assertRaises(urllib.error.HTTPError):
                    release.registry_json('https://example.invalid')
        with patch('urllib.request.urlopen', side_effect=urllib.error.HTTPError('https://example.invalid', 404, 'missing', {}, None)):
            self.assertIsNone(release.registry_json('https://example.invalid'))

    def test_npm_channel_never_moves_backwards(self):
        package = dict(self.catalog['cli'], version='1.2.3', channel='stable')
        with self.assertRaises(ValueError):
            release.guard_npm_channel(package, lambda _: {'version': '1.2.4'})
        release.guard_npm_channel(package, lambda _: {'version': '1.2.2'})
        release.guard_npm_channel(package, lambda _: None)
        self.assertGreater(release.version_order('1.2.3'), release.version_order('1.2.3-rc.4'))
        self.assertGreater(release.version_order('1.2.3-beta.10'), release.version_order('1.2.3-beta.2'))

    def test_go_retry_never_moves_tags(self):
        package = dict(self.catalog['go'], version='1.2.3', tag='sdks/go/workflows/v1.2.3')
        with patch.object(release, 'run', side_effect=['abc\n', 'abc\trefs/tags/sdks/go/workflows/v1.2.3\n']) as run:
            release.publish_go([package])
            self.assertEqual(run.call_count, 2)
        with patch.object(release, 'run', side_effect=['abc\n', 'def\trefs/tags/sdks/go/workflows/v1.2.3\n']) as run:
            with self.assertRaises(ValueError):
                release.publish_go([package])
            self.assertEqual(run.call_count, 2)

    def test_server_workflow_does_not_match_sdk_tags(self):
        import yaml
        workflow = yaml.safe_load((release.ROOT / '.github/workflows/release.yml').read_text())
        self.assertEqual(workflow[True]['push']['tags'], ['v*.*.*', 'v*.*.*-*'])


if __name__ == '__main__':
    unittest.main()

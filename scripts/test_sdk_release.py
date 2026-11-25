# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

import argparse
import base64
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import json
import io
import os
import re
import shutil
import subprocess
import tarfile
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
import sdk_prepare
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
        self.write('sha256.json', {})
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

    def build_python(self, *, locked):
        package = {'kind': 'pypi', 'key': 'python', 'path': 'python', 'name': 'temps-workflows', 'version': '1.2.3b1'}
        if locked:
            (self.root / 'python/uv.lock').write_text('version = 1\n')
        def command(*args, **kwargs):
            if args[:2] == ('git', 'show'):
                return '1234567890\n'
            if args[:2] == ('git', 'rev-parse'):
                return 'abc\n'
            if '--outdir' in args:
                (Path(args[args.index('--outdir') + 1]) / 'temps_workflows-1.2.3b1-py3-none-any.whl').write_bytes(b'wheel')
            return ''
        with patch.object(release, 'ROOT', self.root), patch.object(release, 'run', side_effect=command) as run:
            release.build([package], self.root / 'artifacts')
        return [(call.args, call.kwargs.get('cwd')) for call in run.call_args_list if call.args[0] == 'uv']

    def test_python_build_rejects_a_stale_lockfile_before_packaging(self):
        commands = self.build_python(locked=True)
        self.assertEqual(commands[0], (('uv', 'lock', '--check'), self.root / 'python'))
        self.assertIn('build', commands[1][0])

    def test_python_build_without_a_lockfile_skips_the_lock_check(self):
        commands = self.build_python(locked=False)
        self.assertNotIn(('uv', 'lock', '--check'), [args for args, _ in commands])

    def test_api_build_regenerates_its_client_before_testing_and_packing(self):
        # Packing ignores lifecycle scripts, so the generator its prepublishOnly runs must run here.
        package = next(p for p in release.catalog_packages() if p['key'] == 'api')
        self.assertIn('bun run generate', release.manifest(package)['scripts']['prepublishOnly'])
        def command(*args, **kwargs):
            if args[:2] == ('git', 'show'):
                return '1234567890\n'
            if args[:2] == ('git', 'rev-parse'):
                return 'abc\n'
            if args[:3] == ('bun', 'pm', 'pack'):
                Path(args[args.index('--filename') + 1]).write_bytes(b'package')
            return ''
        with patch.object(release, 'run', side_effect=command) as run, patch.object(release, 'check_npm_archive'):
            release.build([package], self.root / 'artifacts')
        calls = [call.args for call in run.call_args_list if call.kwargs.get('cwd') == release.ROOT / 'packages/api']
        self.assertLess(calls.index(('bun', 'run', 'generate')), calls.index(('bun', 'run', 'build')))
        packed = next(c for c in calls if c[:3] == ('bun', 'pm', 'pack'))
        self.assertLess(calls.index(('bun', 'run', 'generate')), calls.index(packed))
        # Packages without a generating prepublishOnly are packed from their committed sources.
        kv = next(p for p in release.catalog_packages() if p['key'] == 'kv')
        with patch.object(release, 'run', side_effect=command) as run, patch.object(release, 'check_npm_archive'):
            release.build([kv], self.root / 'artifacts-kv')
        self.assertNotIn(('bun', 'run', 'generate'), [call.args for call in run.call_args_list])

    def test_only_the_generate_script_itself_counts_as_regenerating(self):
        for script, expected in [('bun run generate && bun run build', True), ('bun run generate', True),
                                 ('tsc; bun run generate', True), ('bun run generate:types', False),
                                 ('bun run generated', False), ('bun run build', False), ('', False)]:
            with self.subTest(script=script):
                self.assertIs(release.runs_generate(script), expected)

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

    def test_cli_is_available_for_main_release_selection(self):
        catalog = release.read_json(release.ROOT / '.github/releases/packages.json')
        self.assertEqual(catalog['cli'], {
            'kind': 'npm', 'path': 'apps/temps-cli', 'name': '@temps-sdk/cli',
            'stamped': ['skills/temps-cli/references/COMMANDS.md'],
            'channel_pinned': ['skills/**/*.md', 'skills/**/*.json'],
            # 0.1.38 and 0.2.0-beta.1 shipped before SDKs followed the server version.
            'versioning': 'until-server-passes'})
        # Every other SDK releases at the server version.
        self.assertEqual({key for key, entry in catalog.items() if entry.get('versioning', 'server') != 'server'},
                         {'cli'})
        package = next(p for p in release.catalog_packages() if p['key'] == 'cli')
        self.assertEqual(release.plan(package['tag']), [package])
        # The generated command reference names the current CLI version, so `prepare` can move it.
        reference = (release.ROOT / 'skills/temps-cli/references/COMMANDS.md').read_text()
        self.assertIn(f"`@temps-sdk/cli@{package['version']}`", reference)

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
            with patch.object(release.time, 'sleep'), patch('urllib.request.urlopen', side_effect=urllib.error.HTTPError('https://example.invalid', status, 'failure', {}, None)):
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
            release.publish_tags([package])
            self.assertEqual(run.call_count, 2)
        with patch.object(release, 'run', side_effect=['abc\n', 'def\trefs/tags/sdks/go/workflows/v1.2.3\n']) as run:
            with self.assertRaises(ValueError):
                release.publish_tags([package])
            self.assertEqual(run.call_count, 2)

    def test_artifact_tampering_is_rejected(self):
        package = dict(self.catalog['cli'], key='cli', version='1.2.3')
        directory = self.root / 'cli'
        artifact = directory / 'package.tgz'
        artifact.write_bytes(b'tested')
        (directory / 'package.json').unlink()
        self.write('plan.json', [package])
        self.write('sha256.json', {'cli/package.tgz': release.digest(artifact)})
        (self.root / 'revision').write_text('abc')
        release.verify_artifact_plan(self.root, [package], 'abc')
        artifact.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'checksums'):
            release.verify_artifact_plan(self.root, [package], 'abc')

    def test_missing_artifacts_never_count_as_already_published(self):
        directory = self.root / 'empty'
        directory.mkdir()
        for kind in ('crate', 'pypi', 'npm'):
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                release.pending_files({'name': 'sample', 'version': '1.0.0', 'kind': kind}, directory)

    def test_registry_retries_transient_failures_but_not_auth(self):
        error = urllib.error.HTTPError('https://example.invalid', 503, 'unavailable', {}, io.BytesIO())
        response = io.BytesIO(b'{"version":"1.2.3"}')
        with patch.object(release.time, 'sleep') as sleep, patch('urllib.request.urlopen', side_effect=[error, response]) as fetch:
            self.assertEqual(release.registry_json('https://example.invalid'), {'version': '1.2.3'})
            self.assertEqual(fetch.call_count, 2)
            sleep.assert_called_once_with(1)
        error = urllib.error.HTTPError('https://example.invalid', 401, 'unauthorized', {}, io.BytesIO())
        with patch.object(release.time, 'sleep') as sleep, patch('urllib.request.urlopen', side_effect=error) as fetch:
            with self.assertRaises(urllib.error.HTTPError):
                release.registry_json('https://example.invalid')
            self.assertEqual(fetch.call_count, 1)
            sleep.assert_not_called()

    def test_tag_preflight_never_creates_tags(self):
        package = dict(self.catalog['cli'], version='1.2.3', tag='sdk/cli/v1.2.3')
        with patch.object(release, 'run', side_effect=['abc', '']) as command:
            release.publish_tags([package], check_only=True)
            self.assertEqual(command.call_count, 2)
        with patch.object(release, 'run', side_effect=['abc', '', '']) as command:
            release.publish_tags([package])
            self.assertEqual(command.call_args.args[:4], ('gh', 'api', '--method', 'POST'))
            self.assertIn('sha=abc', command.call_args.args)
        with patch.object(release, 'run', side_effect=['abc', 'tag-object\trefs/tags/sdk/cli/v1.2.3\nabc\trefs/tags/sdk/cli/v1.2.3^{}\n']) as command:
            release.publish_tags([package])
            self.assertEqual(command.call_count, 2)

    def test_publication_rechecks_and_skips_identical_existing_upload(self):
        package = dict(self.catalog['cli'], key='cli', version='1.2.3', channel='stable')
        with patch.object(release, 'pending_files', return_value=[]), patch.object(release, 'run') as command:
            release.publish([package], self.root, 'npm')
            command.assert_not_called()

    def npm_packages(self, *keys):
        return [dict(key=key, name=f'@temps-sdk/{key}', kind='npm', version='1.3.0-beta.1', channel='beta')
                for key in keys]

    def test_npm_versions_are_staged_for_approval_never_published_directly(self):
        packages = self.npm_packages('analytics-core', 'react-analytics')
        commands = []

        def command(*args, **kwargs):
            commands.append(args)
            return '11.19.1\n' if args == ('npm', '--version') else None

        out = io.StringIO()
        with patch.object(release, 'pending_files', return_value=[Path('package.tgz')]), \
                patch.object(release, 'guard_npm_channel'), patch.object(release, 'verify_registry') as verify, \
                patch.object(release, 'run', side_effect=command), redirect_stdout(out):
            release.publish(packages, self.root, 'npm')
        staged = [c for c in commands if c[:3] == ('npm', 'stage', 'publish')]
        self.assertEqual(len(staged), 2)
        self.assertFalse([c for c in commands if c[:2] == ('npm', 'publish')])
        self.assertIn('--provenance', staged[0])
        self.assertEqual(staged[0][staged[0].index('--tag') + 1], 'beta')
        # A staged version is not public yet, so nothing waits for the registry here.
        verify.assert_not_called()
        log = out.getvalue()
        self.assertIn('staged on npm, waiting for approval', log)
        self.assertIn('@temps-sdk/analytics-core@1.3.0-beta.1, @temps-sdk/react-analytics@1.3.0-beta.1', log)

    def test_npm_without_stage_support_uploads_nothing(self):
        for version in ('11.15.0', '10.9.2', 'not-a-version'):
            with patch.object(release, 'pending_files', return_value=[Path('package.tgz')]), \
                    patch.object(release, 'guard_npm_channel'), \
                    patch.object(release, 'run', return_value=version) as command:
                with self.assertRaisesRegex(ValueError, 'cannot stage packages.*11.16.0'):
                    release.publish(self.npm_packages('kv'), self.root, 'npm')
                command.assert_called_once_with('npm', '--version', capture=True)
        self.assertEqual(release.npm_version_tuple('11.16.0'), (11, 16, 0))
        self.assertEqual(release.npm_version_tuple('12.2.3-pre.1'), (12, 2, 3))

    def test_a_channel_refusal_stages_nothing(self):
        packages = self.npm_packages('kv', 'blob')

        def guard(package):
            if package['key'] == 'blob':
                raise ValueError('refusing to move beta backwards')

        with patch.object(release, 'pending_files', return_value=[Path('package.tgz')]), \
                patch.object(release, 'guard_npm_channel', side_effect=guard), \
                patch.object(release, 'run', return_value='11.19.1') as command:
            with self.assertRaisesRegex(ValueError, 'backwards'):
                release.publish(packages, self.root, 'npm')
        self.assertFalse([c for c in command.call_args_list if c.args[:2] == ('npm', 'stage')])

    def test_one_refused_stage_does_not_leave_the_rest_unstaged(self):
        packages = self.npm_packages('kv', 'blob', 'plugin')
        staged = []

        def command(*args, **kwargs):
            if args == ('npm', '--version'):
                return '11.19.1'
            if Path(args[3]).parent.name == 'kv':
                raise subprocess.CalledProcessError(1, args)
            staged.append(args[3])

        with patch.object(release, 'pending_files', side_effect=lambda p, d, *_: [d / 'package.tgz']), \
                patch.object(release, 'guard_npm_channel'), patch.object(release, 'run', side_effect=command), \
                redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, r'refused to stage @temps-sdk/kv@1\.3\.0-beta\.1\..*already exists'):
                release.publish(packages, self.root, 'npm')
        self.assertEqual([Path(a).parent.name for a in staged], ['blob', 'plugin'])

    def test_a_refused_dependency_keeps_its_place_in_the_approval_order(self):
        packages = self.npm_packages('analytics-core', 'react-analytics')

        def command(*args, **kwargs):
            if args == ('npm', '--version'):
                return '11.19.1'
            if Path(args[3]).parent.name == 'analytics-core':
                raise subprocess.CalledProcessError(1, args)

        out = io.StringIO()
        with patch.object(release, 'pending_files', side_effect=lambda p, d, *_: [d / 'package.tgz']), \
                patch.object(release, 'guard_npm_channel'), patch.object(release, 'run', side_effect=command), \
                redirect_stdout(out):
            with self.assertRaisesRegex(ValueError, 'refused to stage @temps-sdk/analytics-core'):
                release.publish(packages, self.root, 'npm')
        self.assertRegex(out.getvalue(), r'in this order so dependencies are public first: '
                                         r'@temps-sdk/analytics-core@1\.3\.0-beta\.1 \(npm refused to stage it in this run\), '
                                         r'@temps-sdk/react-analytics@1\.3\.0-beta\.1\. '
                                         r'Before approving anything listed after a refused version')

    def test_package_tags_wait_until_npm_versions_are_approved(self):
        packages = self.npm_packages('analytics-core', 'react-analytics') + [
            dict(self.catalog['python'], key='python', version='1.3.0b1')]
        with patch.object(release, 'pending_files', return_value=[Path('package.tgz')]) as pending:
            with self.assertRaisesRegex(ValueError, 'no package tag was created.*in this order.*analytics-core.*react-analytics'):
                release.require_npm_approved(packages, self.root)
            self.assertEqual(pending.call_count, 2, 'only npm packages wait for approval')
        with patch.object(release, 'pending_files', return_value=[]):
            release.require_npm_approved(packages, self.root)

    def test_publish_tags_command_checks_npm_approval_before_any_tag(self):
        packages = self.npm_packages('kv')
        with patch.object(release.sys, 'argv', ['sdk_release.py', 'publish-tags', '--tag', 'v1.3.0-beta.1']), \
                patch.object(release, 'plan', return_value=packages), \
                patch.object(release, 'verify_artifact_plan'), patch.object(release, 'run', return_value='abc'), \
                patch.object(release, 'require_npm_approved', side_effect=ValueError('Not public on npm yet')), \
                patch.object(release, 'publish_tags') as tags:
            with self.assertRaisesRegex(ValueError, 'Not public on npm yet'):
                release.main()
            tags.assert_not_called()

    def test_verification_waits_for_registry_visibility_and_is_bounded(self):
        package = dict(self.catalog['cli'], key='cli', version='1.2.3')
        with patch.object(release.time, 'sleep'), patch.object(release, 'pending_files', side_effect=[[Path('package.tgz')], []]) as pending:
            release.verify_registry([package], self.root, 'npm')
            self.assertEqual(pending.call_count, 2)
        with patch.object(release.time, 'sleep'), patch.object(release, 'pending_files', return_value=[Path('package.tgz')]) as pending:
            with self.assertRaisesRegex(ValueError, 'did not expose'):
                release.verify_registry([package], self.root, 'npm')
            self.assertEqual(pending.call_count, 6)

    def test_unselected_workspace_dependency_must_already_be_published(self):
        package = dict(self.catalog['cli'], key='cli', version='1.2.3')
        core = {'name': '@temps-sdk/core', 'kind': 'npm', 'version': '1.0.0'}
        with patch.object(release, 'catalog_packages', return_value=[package, core]), patch.object(release, 'manifest', return_value={'dependencies': {core['name']: 'workspace:*'}}):
            with self.assertRaisesRegex(ValueError, 'prepare unpublished dependency'):
                release.check_dependencies([package], 'npm', lambda _: None)
            release.check_dependencies([package], 'npm', lambda _: {'version': '1.0.0'})
            release.check_dependencies([package, core], 'npm', lambda _: self.fail('Selected dependency queried'))

    def test_cargo_packages_unpublished_siblings_together(self):
        packages = [dict(key=name, name=name, kind='crate', path='crates/' + name, version='1.2.3')
                    for name in ('temps-plugin-protocol', 'temps-plugin-sdk')]
        target = self.root / 'target'
        def command(*args, **kwargs):
            if args[:2] == ('git', 'show'):
                return '1234567890'
            if args[:2] == ('git', 'rev-parse'):
                return 'abc'
            if args[:2] == ('cargo', 'metadata'):
                return json.dumps({'target_directory': str(target)})
            if args[:2] == ('cargo', 'package'):
                (target / 'package').mkdir(parents=True)
                for p in packages:
                    (target / 'package' / (p['name'] + '-1.2.3.crate')).write_bytes(b'tested')
            return ''
        with patch.object(release, 'run', side_effect=command) as cmd:
            release.build(packages, self.root / 'artifacts')
            calls = [c.args for c in cmd.call_args_list if c.args[:2] == ('cargo', 'package')]
            self.assertEqual(calls, [('cargo', 'package', '--locked', '-p', 'temps-plugin-protocol', '-p', 'temps-plugin-sdk')])

    def exercise_rust_publication(self, *, mismatch=False, protocol_published=False):
        packages = [dict(key=name, name=name, kind='crate', path='crates/' + name, version='1.2.3')
                    for name in ('temps-plugin-protocol', 'temps-plugin-sdk')]
        output, target = self.root / 'artifacts', self.root / 'target'
        (target / 'package').mkdir(parents=True)
        for package in packages:
            directory = output / package['key']
            directory.mkdir(parents=True)
            filename = package['name'] + '-1.2.3.crate'
            (directory / filename).write_bytes(package['name'].encode())
            (target / 'package' / filename).write_bytes(b'changed' if mismatch and package['name'] == 'temps-plugin-sdk' else package['name'].encode())
        published = {packages[0]['name']} if protocol_published else set()
        uploads = []
        checked_after_upload = []
        cargo_builds = []
        real_pending = release.pending_files
        def pending(package, directory):
            def fetch(url):
                if package['name'] not in published:
                    return None
                if package['name'] in uploads:
                    checked_after_upload.append(package['name'])
                archive = directory / (package['name'] + '-1.2.3.crate')
                return {'version': {'checksum': release.digest(archive), 'yanked': False}}
            return real_pending(package, directory, fetch)
        def command(*args, **kwargs):
            if args[:2] in (('cargo', 'package'), ('cargo', 'publish')):
                cargo_builds.append(args)
            if args[:2] == ('cargo', 'metadata'):
                return json.dumps({'target_directory': str(target)})
            if args[:2] == ('cargo', 'publish'):
                name = args[-1]
                self.assertNotIn(name, published)
                if name == 'temps-plugin-sdk':
                    self.assertIn('temps-plugin-protocol', published)
                uploads.append(name)
                published.add(name)
            return ''
        with patch.object(release, 'run', side_effect=command), patch.object(release, 'pending_files', side_effect=pending):
            if mismatch:
                with self.assertRaisesRegex(ValueError, 'differs from tested archive'):
                    release.publish(packages, output, 'crate')
                self.assertEqual(uploads, [])
            else:
                release.publish(packages, output, 'crate')
                expected = ['temps-plugin-sdk'] if protocol_published else [p['name'] for p in packages]
                self.assertEqual(uploads, expected)
                self.assertTrue(set(expected).issubset(checked_after_upload))
                # A second invocation reconciles the completed upload without duplicating it.
                release.publish(packages, output, 'crate')
                self.assertEqual(uploads, expected)
        # The publishing job holds registry credentials: it must never compile
        # the locked dependency graph (build scripts, proc macros) again.
        self.assertTrue(cargo_builds)
        for args in cargo_builds:
            self.assertIn('--no-verify', args, f'{args} would compile with registry credentials')

    def test_rust_uploads_verified_archives_in_dependency_order(self):
        self.exercise_rust_publication()

    def test_rust_archive_mismatch_prevents_all_uploads(self):
        self.exercise_rust_publication(mismatch=True)

    def test_rust_retry_after_only_protocol_was_uploaded(self):
        self.exercise_rust_publication(protocol_published=True)

    def test_release_preparation_updates_lockfile_and_signs_commit(self):
        script = (release.ROOT / 'scripts/release.sh').read_text()
        self.assertIn('cargo update --workspace', script)
        self.assertLess(script.index('cargo update --workspace'), script.index('git add Cargo.toml'))
        self.assertIn('git add Cargo.toml crates/temps-cli/Cargo.toml Cargo.lock CHANGELOG.md', script)
        self.assertIn('git commit -s -m', script)

    def test_server_workflow_does_not_match_sdk_tags(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        triggers = workflow.split('  workflow_dispatch:')[0]
        self.assertIn('- "v*.*.*"', triggers)
        self.assertNotIn("sdk/*", triggers)
        publish = workflow.split('  publish-sdks:')[1].split('  build-linux-amd64:')[0]
        self.assertIn('build-web-assets, create-release]', publish)
        self.assertIn("inputs.dry_run != true", publish)
        self.assertNotIn('matrix:', publish)
        self.assertIn('persist-credentials: false', publish)
        self.assertIn('python3 scripts/sdk_release.py publish-tags', publish)
        # An exact Node, so its bundled npm can stage (a cached older Node 24 may not).
        self.assertRegex(publish, r"node-version: '\d+\.\d+\.\d+'")
        self.assertNotIn('npm publish', publish)
        # Registries trust exactly one environment, whatever the channel.
        self.assertIn('    environment: sdk\n', publish)
        self.assertNotIn('sdk_channel }}', publish.split('    concurrency:')[0])


class FakeHistory:
    """In-memory git history: commits oldest first, each touching paths (with contents when known)."""

    def __init__(self):
        self.log = []
        self.refs = {}
        self.unreachable = set()  # tags created on commits after HEAD (a later release)

    def commit(self, subject, files, body=''):
        sha = hashlib.sha1(f'{len(self.log)}:{subject}'.encode()).hexdigest()
        self.log.append({'sha': sha, 'subject': subject, 'body': body,
                         'files': files if isinstance(files, dict) else dict.fromkeys(files)})
        return sha

    def tag(self, name):
        self.refs[name] = self.log[-1]['sha']

    def tags(self, prefix):
        return sorted(tag for tag in self.refs if tag.startswith(prefix))

    def tag_exists(self, tag):
        return tag in self.refs

    def reachable_tags(self):
        return set(self.refs) - self.unreachable

    def resolve(self, ref):
        if ref == 'HEAD':
            return self.log[-1]['sha']
        return self.refs.get(ref.removeprefix('refs/tags/'), '')

    def position(self, revision):
        sha = self.refs.get(revision.removeprefix('refs/tags/'), revision)
        return next(index for index, commit in enumerate(self.log) if commit['sha'] == sha)

    def commits(self, base, paths, excludes):
        def within(file, roots):
            return any(file == root or file.startswith(root + '/') for root in roots)
        return [{k: commit[k] for k in ('sha', 'subject', 'body')}
                for commit in reversed(self.log[self.position(base) + 1:])
                if any(within(file, paths) and not within(file, excludes) for file in commit['files'])]

    def revisions(self, path):
        return [commit['sha'] for commit in reversed(self.log) if path in commit['files']]

    def show(self, revision, path):
        for commit in reversed(self.log[:self.position(revision) + 1]):
            if isinstance(commit['files'].get(path), str):
                return commit['files'][path]
        return None


class PrepareTests(unittest.TestCase):
    """`prepare` against an in-memory history and registry; nothing touches git or the network."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.history = FakeHistory()
        self.registry = {}
        self.catalog = {}
        self.crates = []
        self.write('.github/releases/prepared.json', '{}\n')

    def write(self, path, text):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
        return text

    def read(self, path):
        return (self.root / path).read_text()

    def add(self, key, kind, version, *, name=None, path=None, deps=(), published=(), versioning='independent',
            **extra):
        """Add a catalog package whose manifest an initial commit records at `version`.

        Fixtures default to `independent` so the commit-derived bumps can be tested
        against any server tag; server-versioned tests pass `versioning='server'`.
        """
        extra['versioning'] = versioning
        name = name or {'crate': key, 'go': 'github.com/gotempsh/temps/sdks/go/' + key}.get(kind, '@temps-sdk/' + key)
        path = path or {'npm': f'sdks/node/packages/{key}', 'crate': f'crates/{key}',
                        'pypi': f'sdks/{key}', 'go': f'sdks/go/{key}'}[kind]
        self.catalog[key] = dict(kind=kind, path=path, name=name, **extra)
        self.write('.github/releases/packages.json', json.dumps(self.catalog, indent=2))
        files = {}
        if kind == 'npm':
            data = {'name': name, 'version': version}
            if deps:
                data['dependencies'] = {self.catalog[dep]['name']: 'workspace:*' for dep in deps}
            files[f'{path}/package.json'] = self.write(f'{path}/package.json', json.dumps(data, indent=2) + '\n')
            self.write_bun_lock()
        elif kind == 'crate':
            lines = ['[package]', f'name = "{name}"', f'version = "{version}"', 'edition.workspace = true', '',
                     '[dependencies]', 'serde = { workspace = true }']
            for dep in deps:
                sibling = self.catalog[dep]
                lines.append(f'{sibling["name"]} = {{ path = "../{dep}", version = "={self.version(dep)}" }}')
            files[f'{path}/Cargo.toml'] = self.write(f'{path}/Cargo.toml', '\n'.join(lines) + '\n')
            self.crates.append((name, version, [self.catalog[dep]['name'] for dep in deps]))
            self.write_cargo_lock()
        elif kind == 'pypi':
            files[f'{path}/pyproject.toml'] = self.write(
                f'{path}/pyproject.toml', f'[project]\nname = "{name}"\nversion = "{version}"\ndependencies = []\n')
            self.write(f'{path}/uv.lock', f'version = 1\n\n[[package]]\nname = "{sdk_prepare.canonical(name)}"\n'
                                          f'version = "{version}"\n'
                                          'source = { editable = "." }\n')
        else:
            self.write(f'{path}/go.mod', f'module {name}\n\ngo 1.23\n')
            files[f'{path}/VERSION'] = self.write(f'{path}/VERSION', version + '\n')
        self.history.commit(f'chore(release): add {key}', files)
        self.registry[name] = set(published)

    def version(self, key):
        return release.manifest(self.catalog[key], self.root)['version']

    def write_bun_lock(self):
        """The workspaces section as bun writes it, dependency ranges included."""
        def entry(key, package):
            data = release.manifest(package, self.root)
            deps = ''.join(f'        "{dep}": "{spec}",\n' for dep, spec in data.get('dependencies', {}).items())
            return (f'    "{package["path"].removeprefix("sdks/node/")}": {{\n      "name": "{package["name"]}",\n'
                    f'      "version": "{data["version"]}",\n'
                    + (f'      "dependencies": {{\n{deps}      }},\n' if deps else '') + '    },\n')
        entries = ''.join(entry(key, p) for key, p in sorted(self.catalog.items()) if p['kind'] == 'npm')
        self.write('sdks/node/bun.lock', '{\n  "lockfileVersion": 1,\n  "workspaces": {\n' + entries + '  },\n}\n')

    def write_cargo_lock(self):
        blocks = ['version = 4\n']
        for name, version, deps in self.crates:
            block = f'[[package]]\nname = "{name}"\nversion = "{version}"\n'
            if deps:
                block += 'dependencies = [\n' + ''.join(f' "{dep}",\n' for dep in deps) + ']\n'
            blocks.append(block)
        blocks.append('[[package]]\nname = "serde"\nversion = "1.0.0"\n'
                      'source = "registry+https://github.com/rust-lang/crates.io-index"\n')
        self.write('Cargo.lock', '\n'.join(blocks))

    def release_tag(self, key, version):
        self.history.tag(release.package_tag(key, self.catalog[key], version))

    def change(self, key, subject, body=''):
        return self.history.commit(subject, [f"{self.catalog[key]['path']}/src/index.ts"], body)

    def prepare(self, tag='v9.0.0', write=True):
        return sdk_prepare.prepare(tag, self.root, history=self.history,
                                   published=lambda package: self.registry.get(package['name'], set()), write=write)

    def selection(self, tag='v9.0.0'):
        report, _ = self.prepare(tag)
        return {entry['key']: entry['version'] for entry in report}

    def test_sdks_release_at_the_server_version_with_every_server_release(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'}, versioning='server')
        self.add('react-analytics', 'npm', '0.0.9', deps=['analytics-core'], published={'0.0.9'}, versioning='server')
        self.add('temps-plugin-protocol', 'crate', '0.1.0-beta.62', published={'0.1.0-beta.62'}, versioning='server')
        self.add('temps-plugin-sdk', 'crate', '0.1.0-beta.62', deps=['temps-plugin-protocol'],
                 published={'0.1.0-beta.62'}, versioning='server')
        self.add('py', 'pypi', '0.1.0b1', name='temps-sample', published={'0.1.0b1'}, versioning='server')
        self.add('workflows', 'go', '0.1.0-beta.1', published={'0.1.0-beta.1'}, versioning='server')
        for key in self.catalog:
            self.release_tag(key, self.version(key))
        # Nothing changed since the last release: every SDK still follows the server.
        report, _ = self.prepare('v0.1.0-beta.64')
        self.assertEqual({e['key']: e['version'] for e in report}, {
            'analytics-core': '0.1.0-beta.64', 'react-analytics': '0.1.0-beta.64',
            'temps-plugin-protocol': '0.1.0-beta.64', 'temps-plugin-sdk': '0.1.0-beta.64',
            'py': '0.1.0b64', 'workflows': '0.1.0-beta.64'})
        self.assertEqual({e['reason'] for e in report}, {'server version'})
        self.assertEqual([e['key'] for e in report][:2], ['analytics-core', 'temps-plugin-protocol'])
        self.assertIn('temps-plugin-protocol = { path = "../temps-plugin-protocol", version = "=0.1.0-beta.64" }',
                      self.read('crates/temps-plugin-sdk/Cargo.toml'))
        self.assertIn('name = "temps-plugin-sdk"\nversion = "0.1.0-beta.64"\n', self.read('Cargo.lock'))
        self.assertIn('version = "0.1.0b64"', self.read('sdks/py/uv.lock'))
        self.assertEqual(self.read('sdks/go/workflows/VERSION'), '0.1.0-beta.64\n')
        self.assertEqual(release.channel(self.version('py'), 'pypi'), 'beta')
        self.assertEqual([(p['key'], p['version']) for p in release.plan('v0.1.0-beta.64', self.root)],
                         [(e['key'], e['version']) for e in report])
        # A stable server release takes every SDK stable at the same version.
        self.assertEqual(set(self.selection('v0.1.0').values()), {'0.1.0'})
        self.assertEqual(self.version('py'), '0.1.0')

    def test_commits_are_listed_but_do_not_change_a_server_version(self):
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'}, versioning='server')
        self.release_tag('kv', '0.0.4')
        self.change('kv', 'feat(kv)!: drop the v1 client')
        report, _ = self.prepare('v0.1.0-beta.64')
        self.assertEqual((report[0]['version'], report[0]['reason']), ('0.1.0-beta.64', 'server version; 1 commit (major)'))
        self.assertEqual([c['subject'] for c in report[0]['commits']], ['feat(kv)!: drop the v1 client'])

    def test_a_server_version_already_published_or_passed_fails_closed(self):
        for published, message in [({'0.1.0-beta.64'}, r'@temps-sdk/kv 0\.1\.0-beta\.64 is already published'),
                                   ({'0.1.38'}, r'0\.1\.38 is already published or tagged, above the server version; '
                                                r'set "versioning": "until-server-passes"')]:
            with self.subTest(published=published):
                self.setUp()
                self.add('kv', 'npm', '0.0.4', published=published, versioning='server')
                with self.assertRaisesRegex(ValueError, message):
                    self.prepare('v0.1.0-beta.64')
                self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')
                self.assertEqual(self.version('kv'), '0.0.4')

    def test_a_package_ahead_of_the_server_keeps_its_own_line_until_the_server_passes_it(self):
        self.write('skills/cli/SKILL.md', 'Run `bunx @temps-sdk/cli@beta --help`.\n')
        self.add('cli', 'npm', '0.2.0-beta.1', published={'0.1.38', '0.2.0-beta.1'},
                 versioning='until-server-passes', channel_pinned=['skills/**/*.md'])
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'}, versioning='server')
        self.release_tag('cli', '0.2.0-beta.1')
        # The server (0.1.0-beta.64) is behind the CLI's 0.2.0-beta.1: no CLI change, no CLI release.
        self.assertEqual(self.selection('v0.1.0-beta.64'), {'kv': '0.1.0-beta.64'})
        self.change('cli', 'fix: retry logins')
        self.assertEqual(self.selection('v0.1.0-beta.65'), {'cli': '0.2.0-beta.2', 'kv': '0.1.0-beta.65'})
        self.release_tag('cli', '0.2.0-beta.2')
        # Equal is not past: 0.2.0-beta.2 is already the CLI's, so it keeps its own line.
        self.assertEqual(self.selection('v0.2.0-beta.2'), {'kv': '0.2.0-beta.2'})
        # Once the server is above everything the CLI published, it follows the server, changes or not.
        report, _ = self.prepare('v0.2.0-beta.3')
        self.assertEqual({e['key']: (e['version'], e['reason']) for e in report},
                         {'cli': ('0.2.0-beta.3', 'server version'), 'kv': ('0.2.0-beta.3', 'server version')})

    def test_registry_versions_are_ordered_by_each_registry_s_rules(self):
        semver = ['0.1.0-beta.64', '1.0.0-alpha', '1.0.0-alpha.1', '1.0.0-alpha.beta', '1.0.0-beta', '1.0.0-beta.2',
                  '1.0.0-beta.11', '1.0.0-canary.1', '1.0.0-rc.1', '1.0.0', '1.0.1-0', '1.0.1']
        pep440 = ['0.1.0b64', '1.0.dev1', '1.0a1', '1.0b1.dev1', '1.0b1', '1.0b1.post1', '1.0rc1',
                  '1.0.post1.dev1', '1.0.post1', '1.1.dev1']
        for kind, ordered in [('npm', semver), ('crate', semver), ('go', semver), ('pypi', pep440)]:
            with self.subTest(kind=kind):
                keys = [sdk_prepare.precedence(text, kind) for text in ordered]
                self.assertEqual(sorted(keys), keys)
                self.assertEqual(len(set(keys)), len(keys))
        self.assertEqual(sdk_prepare.precedence('1.0', 'pypi'), sdk_prepare.precedence('1.0.0', 'pypi'))
        self.assertEqual(sdk_prepare.precedence('1.0.0+build.5', 'npm'), sdk_prepare.precedence('1.0.0', 'npm'))
        self.assertLess(sdk_prepare.precedence('1.0rc1', 'pypi'), sdk_prepare.precedence('1.0', 'pypi'))
        for text, kind in [('1!0.1.0', 'pypi'), ('01.0.0', 'npm'), ('1.0', 'npm'), ('latest', 'crate')]:
            with self.subTest(text=text):
                self.assertIsNone(sdk_prepare.precedence(text, kind))

    def test_a_registry_prerelease_prepare_cannot_parse_still_counts_as_published(self):
        # npm accepts any SemVer prerelease label; 0.3.0-canary.1 is above the server's 0.2.0-beta.1.
        self.add('cli', 'npm', '0.1.38', published={'0.1.38', '0.3.0-canary.1'}, versioning='until-server-passes')
        self.release_tag('cli', '0.1.38')
        self.assertEqual(self.selection('v0.2.0-beta.1'), {})
        # canary sorts above beta of the same core, so 0.3.0-beta.1 has not passed it either.
        self.assertEqual(self.selection('v0.3.0-beta.1'), {})
        self.assertEqual(self.selection('v0.3.0'), {'cli': '0.3.0'})
        self.setUp()
        self.add('kv', 'npm', '0.0.4', published={'0.0.4', '0.3.0-canary.1'}, versioning='server')
        with self.assertRaisesRegex(ValueError, r'@temps-sdk/kv 0\.2\.0-beta\.1: 0\.3\.0-canary\.1 is already '
                                                r'published or tagged, above the server version'):
            self.prepare('v0.2.0-beta.1')

    def test_a_pypi_development_release_above_the_server_version_fails_closed(self):
        self.add('py', 'pypi', '0.1.0b1', name='temps-sample', published={'0.1.0b1', '0.2.0.dev1'},
                 versioning='server')
        with self.assertRaisesRegex(ValueError, r'temps-sample 0\.1\.0b64: 0\.2\.0\.dev1 is already published'):
            self.prepare('v0.1.0-beta.64')
        self.assertEqual(self.version('py'), '0.1.0b1')
        # A development release below the server version (0.1.0.dev1 precedes 0.1.0b64) is no obstacle.
        self.registry['temps-sample'] = {'0.1.0b1', '0.1.0.dev1'}
        self.assertEqual(self.selection('v0.1.0-beta.64'), {'py': '0.1.0b64'})

    def test_a_registry_version_that_cannot_be_ordered_stops_preparation(self):
        self.add('py', 'pypi', '0.1.0b1', name='temps-sample', published={'0.1.0b1', '1!0.0.1'}, versioning='server')
        with self.assertRaisesRegex(ValueError, r"temps-sample: cannot order registry version '1!0\.0\.1' "
                                                r'against 0\.1\.0b64'):
            self.prepare('v0.1.0-beta.64')
        self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')

    def test_a_package_tag_prepare_cannot_parse_still_keeps_the_own_line(self):
        # A tag whose upload never happened: 0.3.0-canary.1 is above the server's 0.2.0-beta.1.
        self.add('cli', 'npm', '0.1.38', published={'0.1.38'}, versioning='until-server-passes')
        self.release_tag('cli', '0.1.38')
        self.release_tag('cli', '0.3.0-canary.1')
        self.assertEqual(self.selection('v0.2.0-beta.1'), {})
        self.assertEqual(self.selection('v0.3.0'), {'cli': '0.3.0'})
        self.release_tag('cli', 'next')
        with self.assertRaisesRegex(ValueError, r"@temps-sdk/cli: cannot order package tag 'next' against 0\.3\.1"):
            self.prepare('v0.3.1')

    def test_a_registry_version_differing_only_in_build_metadata_is_taken(self):
        self.add('temps-plugin-protocol', 'crate', '0.1.0-beta.62',
                 published={'0.1.0-beta.62', '0.1.0-beta.64+build.5'}, versioning='server')
        with self.assertRaisesRegex(ValueError, r'temps-plugin-protocol 0\.1\.0-beta\.64 is already published'):
            self.prepare('v0.1.0-beta.64')
        self.assertEqual(self.selection('v0.1.0-beta.65'), {'temps-plugin-protocol': '0.1.0-beta.65'})

    def test_an_older_tag_is_verified_without_a_later_release_prepare_cannot_parse(self):
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'}, versioning='server')
        self.prepare_and_tag('v0.1.0-beta.64')
        self.registry['@temps-sdk/kv'].add('0.1.0-beta.64')
        self.release_tag('kv', '0.1.0-beta.64')
        # A later release, from a commit after this tag, published a canary.
        self.registry['@temps-sdk/kv'].add('0.3.0-canary.1')
        self.release_tag('kv', '0.3.0-canary.1')
        self.history.unreachable.add('sdk/kv/v0.3.0-canary.1')
        self.history.tag('v0.1.0-beta.64')
        self.assertEqual(self.verify('v0.1.0-beta.64'), ([], ['kv']))
        # A canary no release tagged is still above the server version and stops the check.
        self.registry['@temps-sdk/kv'].add('0.2.0-canary.1')
        with self.assertRaisesRegex(ValueError, r'0\.2\.0-canary\.1 is already published or tagged, above'):
            self.verify('v0.1.0-beta.64')

    def test_independent_packages_are_not_released_by_the_server_version(self):
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'})
        self.release_tag('kv', '0.0.4')
        self.assertEqual(self.selection('v0.1.0-beta.64'), {})

    def test_server_tags_sdks_cannot_follow_are_refused(self):
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'}, versioning='server')
        with self.assertRaisesRegex(ValueError, r'v9\.0\.0-test: SDKs release at the server version'):
            self.prepare('v9.0.0-test')

    def test_unknown_versioning_mode_is_rejected(self):
        self.add('kv', 'npm', '0.0.4', versioning='sometimes')
        with self.assertRaisesRegex(ValueError, "kv: unknown versioning 'sometimes'; use one of server, independent, "
                                                'until-server-passes'):
            release.catalog_packages(self.root)

    def test_server_versioned_selection_is_verified_at_its_tag(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'}, versioning='server')
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'}, versioning='server')
        self.prepare_and_tag('v0.1.0-beta.64')
        self.assertEqual(self.verify('v0.1.0-beta.64'), ([], []))
        # A re-run after kv was published still verifies against the registry as it was before.
        self.registry['@temps-sdk/kv'].add('0.1.0-beta.64')
        self.release_tag('kv', '0.1.0-beta.64')
        self.history.tag('v0.1.0-beta.64')
        self.assertEqual(self.verify('v0.1.0-beta.64'), ([], ['kv']))

    def test_conventional_commit_levels(self):
        for subject, body, level in [
                ('fix(cli): handle empty input', '', 'patch'), ('perf: cache lookups', '', 'patch'),
                ('feat(kv): add scan', '', 'minor'), ('feat(kv)!: drop the v1 client', '', 'major'),
                ('fix!: rename the option', '', 'major'),
                ('refactor(kv): split modules', 'BREAKING CHANGE: the client is now async', 'major'),
                ('fix: tidy', 'BREAKING-CHANGE: renamed', 'major'),
                ('chore(deps): bump serde', '', None), ('docs: explain', '', None), ('test: cover', '', None),
                ('Merge pull request #1 from someone/branch', '', None), ('feat:missing space', '', None),
                ('Fix: capitalised type', '', 'patch'), ('FEAT(kv): shouting', '', 'minor'),
                ('docs: lowercase footer', 'breaking change: not a footer', None),
                ('fix: mentions BREAKING CHANGE in passing', '', 'patch')]:
            with self.subTest(subject=subject):
                self.assertEqual(sdk_prepare.commit_level(subject, body), level)

    def test_each_bump_kind_from_a_tagged_release(self):
        for subject, expected in [('fix: patch', '1.2.4'), ('perf: faster', '1.2.4'), ('feat: feature', '1.3.0'),
                                  ('feat!: breaking', '2.0.0'), ('chore: nothing', None)]:
            with self.subTest(subject=subject):
                self.setUp()
                self.add('kv', 'npm', '1.2.3', published={'1.2.3'})
                self.release_tag('kv', '1.2.3')
                self.change('kv', subject)
                self.change('kv', 'fix: second fix')
                if expected is None:
                    self.history.log.pop()
                self.assertEqual(self.selection().get('kv'), expected)

    def test_pre_1_0_shifts_bumps_down_one_level(self):
        for subjects, expected in [(['feat!: breaking'], '0.5.0'), (['feat: feature'], '0.4.3'),
                                   (['fix: patch'], '0.4.3'), (['fix: one', 'feat: two'], '0.4.3')]:
            with self.subTest(subjects=subjects):
                self.setUp()
                self.add('kv', 'npm', '0.4.2', published={'0.4.2'})
                self.release_tag('kv', '0.4.2')
                for subject in subjects:
                    self.change('kv', subject)
                self.assertEqual(self.selection().get('kv'), expected)
                self.assertEqual(json.loads(self.read('sdks/node/packages/kv/package.json'))['version'], expected)

    def test_next_core_follows_semver_increments_of_prereleases(self):
        version = sdk_prepare.Version
        for base, level, expected in [('1.2.3', 'patch', '1.2.4'), ('1.2.3', 'minor', '1.3.0'),
                                      ('1.2.3', 'major', '2.0.0'), ('1.3.0-beta.2', 'minor', '1.3.0'),
                                      ('1.2.3-beta.2', 'minor', '1.3.0'), ('2.0.0-beta.1', 'major', '2.0.0'),
                                      ('1.2.0-beta.1', 'major', '2.0.0'), ('1.2.3-beta.4', 'patch', '1.2.3'),
                                      ('0.1.0-beta.62', 'major', '0.1.0'), ('0.0.1-beta.1', 'major', '0.1.0'),
                                      ('1.2.3-beta.4', None, '1.2.3')]:
            with self.subTest(base=base, level=level):
                self.assertEqual(sdk_prepare.next_core(version.parse(base, 'npm'), level).format('npm'), expected)

    def test_beta_numbering_continues_from_registry_and_tags(self):
        self.add('kv', 'npm', '1.2.3', published={'1.2.3', '1.3.0-beta.1', '1.3.0-beta.2'})
        self.add('py', 'pypi', '1.2.3', name='temps-sample', published={'1.2.3', '1.3.0b1'})
        self.release_tag('kv', '1.2.3')
        self.release_tag('py', '1.2.3')
        self.change('kv', 'feat: scan')
        self.change('py', 'feat: scan')
        self.history.commit('chore: tag a beta whose upload never happened', ['README.md'])
        self.release_tag('kv', '1.3.0-beta.4')
        self.change('kv', 'fix: scan bound')
        self.assertEqual(self.selection('v9.0.0-beta.1'), {'kv': '1.3.0-beta.5', 'py': '1.3.0b2'})
        self.assertEqual(release.channel(self.version('py'), 'pypi'), 'beta')

    def test_beta_after_a_beta_tag_keeps_its_core_and_stable_graduates_it(self):
        self.add('kv', 'npm', '1.3.0-beta.1', published={'1.2.3', '1.3.0-beta.1'})
        self.release_tag('kv', '1.3.0-beta.1')
        self.change('kv', 'fix: follow-up')
        self.assertEqual(self.selection('v9.0.0-beta.2'), {'kv': '1.3.0-beta.2'})
        self.setUp()
        self.add('kv', 'npm', '1.3.0-beta.1', published={'1.2.3', '1.3.0-beta.1'})
        self.release_tag('kv', '1.3.0-beta.1')
        # No commit since the beta: a stable server tag still releases it as stable.
        self.assertEqual(self.selection('v9.0.0'), {'kv': '1.3.0'})

    def test_untagged_unpublished_package_is_a_first_release(self):
        self.add('sandbox', 'npm', '0.0.1')
        self.add('protocol', 'crate', '0.1.0-beta.62', name='temps-plugin-protocol')
        self.add('py', 'pypi', '0.1.0', name='temps-sample')
        self.add('workflows', 'go', '0.1.0')
        beta = self.selection('v9.0.0-beta.1')
        self.assertEqual(beta, {'protocol': '0.1.0-beta.62', 'py': '0.1.0b1',
                                'sandbox': '0.0.1-beta.1', 'workflows': '0.1.0-beta.1'})
        self.assertEqual(self.read('sdks/go/workflows/VERSION'), '0.1.0-beta.1\n')
        self.assertIn('version = "0.1.0b1"', self.read('sdks/py/uv.lock'))
        report, _ = self.prepare('v9.0.0-beta.1', write=False)
        self.assertTrue(all(entry['previous'] is None and 'first release' in entry['reason'] for entry in report))

    def test_untagged_unpublished_package_goes_stable_as_its_manifest_core(self):
        self.add('sandbox', 'npm', '0.0.1')
        self.add('protocol', 'crate', '0.1.0-beta.62', name='temps-plugin-protocol')
        self.assertEqual(self.selection('v9.0.0'), {'protocol': '0.1.0', 'sandbox': '0.0.1'})

    def test_already_published_version_without_changes_is_skipped(self):
        self.add('kv', 'npm', '0.0.4', published={'0.0.3', '0.0.4'})
        self.change('kv', 'docs: readme')
        report, files = self.prepare()
        self.assertEqual(report, [])
        self.assertEqual(files, [])
        self.assertEqual(json.loads(self.read('.github/releases/prepared.json')), {})

    def test_untagged_published_package_anchors_on_the_commit_that_set_its_version(self):
        self.history.commit('feat: shipped in 0.0.4', ['sdks/node/packages/kv/src/a.ts'])
        self.add('kv', 'npm', '0.0.4', published={'0.0.4'})
        self.change('kv', 'fix: after the release')
        report, _ = self.prepare()
        self.assertEqual([(e['key'], e['previous'], e['version']) for e in report], [('kv', '0.0.4', '0.0.5')])
        self.assertEqual([c['subject'] for c in report[0]['commits']], ['fix: after the release'])

    def test_a_computed_version_that_is_already_published_is_never_selected(self):
        self.add('kv', 'npm', '1.2.3', published={'1.2.3', '1.2.4'})
        self.release_tag('kv', '1.2.3')
        self.change('kv', 'fix: patch')
        with self.assertRaisesRegex(ValueError, r'@temps-sdk/kv 1\.2\.4 is already published'):
            self.prepare()
        self.assertEqual(self.version('kv'), '1.2.3')

    def test_registry_ahead_of_history_fails_closed(self):
        self.add('kv', 'npm', '0.0.5', published={'0.0.9'})
        with self.assertRaisesRegex(ValueError, 'registry already has 0.0.9'):
            self.prepare()

    def test_dependency_cascade_rebuilds_dependents_in_publish_order(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('react-analytics', 'npm', '0.0.9', deps=['analytics-core'], published={'0.0.9'})
        self.add('temps-plugin-protocol', 'crate', '1.0.0', published={'1.0.0'})
        self.add('temps-plugin-sdk', 'crate', '1.0.0', deps=['temps-plugin-protocol'], published={'1.0.0'})
        self.add('unrelated', 'npm', '1.0.0', published={'1.0.0'})
        for key, version in [('analytics-core', '0.0.6'), ('react-analytics', '0.0.9'),
                             ('temps-plugin-protocol', '1.0.0'), ('temps-plugin-sdk', '1.0.0'), ('unrelated', '1.0.0')]:
            self.release_tag(key, version)
        self.change('analytics-core', 'feat: new event')
        self.history.commit('fix(protocol): header', ['crates/temps-plugin-protocol/src/lib.rs'])
        report, _ = self.prepare()
        self.assertEqual([(e['key'], e['version']) for e in report],
                         [('analytics-core', '0.0.7'), ('temps-plugin-protocol', '1.0.1'),
                          ('react-analytics', '0.0.10'), ('temps-plugin-sdk', '1.0.1')])
        self.assertEqual(report[2]['reason'], 'dependency @temps-sdk/analytics-core')
        self.assertEqual(list(json.loads(self.read('.github/releases/prepared.json'))['v9.0.0']),
                         ['analytics-core', 'temps-plugin-protocol', 'react-analytics', 'temps-plugin-sdk'])
        self.assertEqual([p['key'] for p in release.plan('v9.0.0', self.root)],
                         ['analytics-core', 'temps-plugin-protocol', 'react-analytics', 'temps-plugin-sdk'])
        self.assertIn('temps-plugin-protocol = { path = "../temps-plugin-protocol", version = "=1.0.1" }',
                      self.read('crates/temps-plugin-sdk/Cargo.toml'))
        lock = self.read('Cargo.lock')
        self.assertIn('name = "temps-plugin-protocol"\nversion = "1.0.1"\n', lock)
        self.assertIn('name = "temps-plugin-sdk"\nversion = "1.0.1"\n', lock)
        self.assertIn('name = "serde"\nversion = "1.0.0"\n', lock)
        bun = self.read('sdks/node/bun.lock')
        self.assertIn('"name": "@temps-sdk/analytics-core",\n      "version": "0.0.7"', bun)
        self.assertIn('"name": "@temps-sdk/react-analytics",\n      "version": "0.0.10"', bun)
        self.assertIn('"name": "@temps-sdk/unrelated",\n      "version": "1.0.0"', bun)

    def test_cascade_is_transitive(self):
        self.add('base', 'npm', '1.0.0', published={'1.0.0'})
        self.add('middle', 'npm', '1.0.0', deps=['base'], published={'1.0.0'})
        self.add('top', 'npm', '1.0.0', deps=['middle'], published={'1.0.0'})
        for key in ('base', 'middle', 'top'):
            self.release_tag(key, '1.0.0')
        self.change('base', 'fix: bug')
        self.assertEqual(self.selection(), {'base': '1.0.1', 'middle': '1.0.1', 'top': '1.0.1'})

    def test_stable_package_cannot_depend_on_a_prerelease_sibling(self):
        self.add('core', 'npm', '1.0.0', published={'1.0.0'})
        self.add('app', 'npm', '1.0.0', deps=['core'], published={'1.0.0'})
        self.release_tag('core', '1.0.0')
        self.release_tag('app', '1.0.0')
        # A hand edit left the core on an unpublished prerelease with nothing releasable since its tag.
        path = 'sdks/node/packages/core/package.json'
        self.history.commit('chore: try a beta', {path: self.write(
            path, json.dumps({'name': '@temps-sdk/core', 'version': '1.1.0-beta.1'}) + '\n')})
        self.change('app', 'fix: app bug')
        with self.assertRaisesRegex(ValueError, 'app: stable package depends on prerelease sibling @temps-sdk/core'):
            self.prepare('v9.0.0')
        self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')
        # A beta release of app cannot ship against a core version the registry lacks either.
        with self.assertRaisesRegex(ValueError, 'app: depends on @temps-sdk/core 1.1.0-beta.1, which is neither published'):
            self.prepare('v9.0.0-beta.1')
        self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')

    def test_a_bump_rewrites_the_version_stamped_in_generated_files(self):
        reference = 'skills/cli/COMMANDS.md'
        self.write(reference, '> Generated from: `@temps-sdk/cli@0.1.38`\n\n`@temps-sdk/cli@0.1.380` is another version.\n')
        self.add('cli', 'npm', '0.1.38', published={'0.1.38'}, stamped=[reference])
        self.change('cli', 'feat: deploy previews')
        _, files = self.prepare('v9.0.0-beta.1')
        self.assertIn(reference, files)
        self.assertEqual(self.read(reference), '> Generated from: `@temps-sdk/cli@0.1.39-beta.1`\n\n'
                                               '`@temps-sdk/cli@0.1.380` is another version.\n')

    def test_a_stale_version_stamp_fails_before_anything_is_written(self):
        reference = 'skills/cli/COMMANDS.md'
        self.write(reference, '> Generated from: `@temps-sdk/cli@0.1.37`\n')
        self.add('cli', 'npm', '0.1.38', published={'0.1.38'}, stamped=[reference])
        self.change('cli', 'feat: deploy previews')
        with self.assertRaisesRegex(ValueError, r'COMMANDS\.md: expected 1 @temps-sdk/cli@0\.1\.38 stamp, found 0'):
            self.prepare('v9.0.0-beta.1')
        self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')
        self.assertEqual(self.version('cli'), '0.1.38')

    CHANNEL_PINS = ['skills/**/*.md', 'skills/**/*.json']

    def test_a_beta_bump_moves_the_skills_cli_channel_to_beta(self):
        skill = ('Run `bunx @temps-sdk/cli@latest --help`, or npx @temps-sdk/cli@latest.\n'
                 '`@temps-sdk/cli@latest-preview` and `@temps-sdk/node-sdk@latest` are other specifiers.\n')
        self.write('skills/cli/SKILL.md', skill)
        self.write('skills/cli/evals/evals.json', '{"expect": "Uses @temps-sdk/cli@latest"}\n')
        self.write('skills/other/SKILL.md', 'No CLI here.\n')
        self.write('docs/upgrade.md', 'Upgrade with `bunx @temps-sdk/cli@latest`.\n')
        self.add('cli', 'npm', '0.1.38', published={'0.1.38'}, channel_pinned=self.CHANNEL_PINS)
        self.change('cli', 'feat!: replace the MCP tools')
        _, files = self.prepare('v9.0.0-beta.1')
        self.assertEqual(self.version('cli'), '0.2.0-beta.1')
        self.assertIn('skills/cli/SKILL.md', files)
        self.assertIn('skills/cli/evals/evals.json', files)
        self.assertNotIn('skills/other/SKILL.md', files)
        self.assertEqual(self.read('skills/cli/SKILL.md'), skill.replace(
            'cli@latest --help`, or npx @temps-sdk/cli@latest.', 'cli@beta --help`, or npx @temps-sdk/cli@beta.'))
        self.assertEqual(self.read('skills/cli/evals/evals.json'), '{"expect": "Uses @temps-sdk/cli@beta"}\n')
        # Only the globbed skill files move; release notes elsewhere keep what they say.
        self.assertEqual(self.read('docs/upgrade.md'), 'Upgrade with `bunx @temps-sdk/cli@latest`.\n')

    def test_a_stable_release_moves_the_skills_cli_channel_back_to_latest(self):
        self.write('skills/cli/SKILL.md', 'Run `bunx @temps-sdk/cli@beta --help`.\n')
        self.add('cli', 'npm', '0.2.0-beta.1', published={'0.1.38', '0.2.0-beta.1'},
                 channel_pinned=self.CHANNEL_PINS)
        self.change('cli', 'fix: retry logins')
        self.prepare('v9.0.0')
        self.assertEqual(self.version('cli'), '0.2.0')
        self.assertEqual(self.read('skills/cli/SKILL.md'), 'Run `bunx @temps-sdk/cli@latest --help`.\n')

    def test_a_bump_within_a_channel_leaves_the_pins_alone(self):
        self.write('skills/cli/SKILL.md', 'Run `bunx @temps-sdk/cli@beta --help`.\n')
        self.add('cli', 'npm', '0.2.0-beta.1', published={'0.2.0-beta.1'}, channel_pinned=self.CHANNEL_PINS)
        self.change('cli', 'fix: retry logins')
        _, files = self.prepare('v9.0.0-beta.1')
        self.assertEqual(self.version('cli'), '0.2.0-beta.2')
        self.assertNotIn('skills/cli/SKILL.md', files)

    def test_a_channel_change_without_pins_fails_before_anything_is_written(self):
        self.write('skills/cli/SKILL.md', 'Run `bunx @temps-sdk/cli@beta --help`.\n')
        self.add('cli', 'npm', '0.1.38', published={'0.1.38'}, channel_pinned=self.CHANNEL_PINS)
        self.change('cli', 'feat: deploy previews')
        with self.assertRaisesRegex(ValueError, r'cli: no file matching skills/\*\*/\*\.md, skills/\*\*/\*\.json '
                                                r'pins @temps-sdk/cli@latest'):
            self.prepare('v9.0.0-beta.1')
        self.assertEqual(self.read('.github/releases/prepared.json'), '{}\n')
        self.assertEqual(self.version('cli'), '0.1.38')

    def test_prepare_is_idempotent(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('react-analytics', 'npm', '0.0.9', deps=['analytics-core'], published={'0.0.9'})
        self.add('sandbox', 'npm', '0.0.1')
        self.add('temps-plugin-protocol', 'crate', '1.0.0', published={'1.0.0'})
        self.add('temps-plugin-sdk', 'crate', '1.0.0', deps=['temps-plugin-protocol'], published={'1.0.0'})
        self.add('py', 'pypi', '0.1.0', name='temps-sample')
        self.release_tag('temps-plugin-protocol', '1.0.0')
        self.release_tag('temps-plugin-sdk', '1.0.0')
        self.change('analytics-core', 'feat: event')
        self.history.commit('feat(protocol)!: v2 frames', ['crates/temps-plugin-protocol/src/lib.rs'])
        for tag in ('v9.0.0-beta.1', 'v9.0.0'):
            with self.subTest(tag=tag):
                _, first = self.prepare(tag)
                self.assertTrue(first)
                snapshot = {path: self.read(path) for path in first}
                again, second = self.prepare(tag, write=False)
                self.assertEqual(second, [])
                self.prepare(tag)
                self.assertEqual({path: self.read(path) for path in first}, snapshot)
                self.assertEqual(release.plan(tag, self.root)[0]['key'], 'analytics-core')
                self.assertEqual(len(again), 6)

    def test_python_and_go_workspace_dependencies_cascade_and_are_repinned(self):
        self.add('py-core', 'pypi', '1.0.0', name='temps_core', published={'1.0.0'})
        self.add('py-app', 'pypi', '1.0.0', name='temps-app', published={'1.0.0'})
        self.write('sdks/py-app/pyproject.toml', '[project]\nname = "temps-app"\nversion = "1.0.0"\n'
                   'dependencies = ["temps-core==1.0.0", "aiohttp>=3"]\n\n'
                   '[tool.uv.sources]\ntemps-core = { path = "../py-core" }\n')
        # The dependent's lock as uv writes it: the sibling is a `directory` entry.
        self.write('sdks/py-app/uv.lock', 'version = 1\n\n[[package]]\nname = "temps-app"\nversion = "1.0.0"\n'
                   'source = { editable = "." }\ndependencies = [\n    { name = "temps-core" },\n]\n\n'
                   '[package.metadata]\nrequires-dist = [{ name = "temps-core", directory = "../py-core" }]\n\n'
                   '[[package]]\nname = "temps-core"\nversion = "1.0.0"\nsource = { directory = "../py-core" }\n')
        self.add('go-core', 'go', '1.0.0', published={'1.0.0'})
        self.add('go-app', 'go', '1.0.0', published={'1.0.0'})
        core = self.catalog['go-core']['name']
        self.write('sdks/go/go-app/go.mod', f'module {self.catalog["go-app"]["name"]}\n\ngo 1.23\n\n'
                   f'require {core} v1.0.0\n\nreplace {core} => ../go-core\n')
        for key in ('py-core', 'py-app', 'go-core', 'go-app'):
            self.release_tag(key, '1.0.0')
        self.change('py-core', 'fix: core')
        self.change('go-core', 'feat: core')
        self.assertEqual(self.selection(), {'go-app': '1.0.1', 'go-core': '1.1.0', 'py-app': '1.0.1', 'py-core': '1.0.1'})
        self.assertIn('"temps-core==1.0.1"', self.read('sdks/py-app/pyproject.toml'))
        app_lock = self.read('sdks/py-app/uv.lock')
        self.assertIn('name = "temps-app"\nversion = "1.0.1"\nsource = { editable = "." }', app_lock)
        self.assertIn('name = "temps-core"\nversion = "1.0.1"\nsource = { directory = "../py-core" }', app_lock)
        self.assertIn(f'require {core} v1.1.0\n\nreplace {core} => ../go-core\n', self.read('sdks/go/go-app/go.mod'))

    def test_renamed_cargo_dependency_is_repinned_and_unrewritable_pins_fail(self):
        self.add('temps-plugin-protocol', 'crate', '1.0.0', published={'1.0.0'})
        self.add('temps-plugin-sdk', 'crate', '1.0.0', published={'1.0.0'})
        manifest = 'crates/temps-plugin-sdk/Cargo.toml'
        self.write(manifest, '[package]\nname = "temps-plugin-sdk"\nversion = "1.0.0"\n\n[dependencies]\n'
                   'wire = { package = "temps-plugin-protocol", path = "../temps-plugin-protocol", version = "=1.0.0" }\n')
        for key in ('temps-plugin-protocol', 'temps-plugin-sdk'):
            self.release_tag(key, '1.0.0')
        self.history.commit('fix(protocol): frame', ['crates/temps-plugin-protocol/src/lib.rs'])
        self.assertEqual(self.selection(), {'temps-plugin-protocol': '1.0.1', 'temps-plugin-sdk': '1.0.1'})
        self.assertIn('wire = { package = "temps-plugin-protocol", path = "../temps-plugin-protocol", version = "=1.0.1" }',
                      self.read(manifest))
        # A dotted table is not rewritten in place, so preparation refuses rather than shipping a stale pin.
        self.setUp()
        self.add('temps-plugin-protocol', 'crate', '1.0.0', published={'1.0.0'})
        self.add('temps-plugin-sdk', 'crate', '1.0.0', published={'1.0.0'})
        self.write(manifest, '[package]\nname = "temps-plugin-sdk"\nversion = "1.0.0"\n\n'
                   '[dependencies.temps-plugin-protocol]\npath = "../temps-plugin-protocol"\nversion = "=1.0.0"\n')
        for key in ('temps-plugin-protocol', 'temps-plugin-sdk'):
            self.release_tag(key, '1.0.0')
        self.history.commit('fix(protocol): frame', ['crates/temps-plugin-protocol/src/lib.rs'])
        with self.assertRaisesRegex(ValueError, 'could not repin temps-plugin-protocol to 1.0.1'):
            self.prepare()
        self.assertEqual(self.read('Cargo.lock').count('version = "1.0.0"'), 3)

    def test_explicit_npm_workspace_versions_are_repinned(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('react-analytics', 'npm', '0.0.9', published={'0.0.9'})
        self.add('vue-analytics', 'npm', '0.0.6', published={'0.0.6'})
        for key, requirement in [('react-analytics', 'workspace:^0.0.6'), ('vue-analytics', 'workspace:*')]:
            path = f'sdks/node/packages/{key}/package.json'
            data = json.loads(self.read(path))
            data['dependencies'] = {'@temps-sdk/analytics-core': requirement}
            self.write(path, json.dumps(data, indent=2) + '\n')
        self.write_bun_lock()
        for key in ('analytics-core', 'react-analytics', 'vue-analytics'):
            self.release_tag(key, self.version(key))
        self.change('analytics-core', 'fix: event')
        self.assertEqual(self.selection(), {'analytics-core': '0.0.7', 'react-analytics': '0.0.10', 'vue-analytics': '0.0.7'})
        react = json.loads(self.read('sdks/node/packages/react-analytics/package.json'))
        vue = json.loads(self.read('sdks/node/packages/vue-analytics/package.json'))
        self.assertEqual(react['dependencies'], {'@temps-sdk/analytics-core': 'workspace:^0.0.7'})
        self.assertEqual(vue['dependencies'], {'@temps-sdk/analytics-core': 'workspace:*'})
        # bun.lock records each dependent's range; it stays equal to the manifest, as
        # a fresh `bun install` would write it, and the new versions are recorded.
        before = self.read('sdks/node/bun.lock')
        self.write_bun_lock()
        self.assertEqual(self.read('sdks/node/bun.lock'), before)
        self.assertIn('"@temps-sdk/analytics-core": "workspace:^0.0.7",', before)
        self.assertNotIn('workspace:^0.0.6', before)

    def test_dependent_bun_lock_ranges_follow_a_kept_manual_bump(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('react-analytics', 'npm', '0.0.9', published={'0.0.9'})
        path = 'sdks/node/packages/react-analytics/package.json'
        data = json.loads(self.read(path))
        data['dependencies'] = {'@temps-sdk/analytics-core': 'workspace:^0.0.6'}
        self.write(path, json.dumps(data, indent=2) + '\n')
        for key in ('analytics-core', 'react-analytics'):
            self.release_tag(key, self.version(key))
        # Someone bumps core by hand to an unpublished 0.1.0 and its bun.lock entry with it.
        core = 'sdks/node/packages/analytics-core/package.json'
        self.write(core, self.read(core).replace('0.0.6', '0.1.0'))
        self.write_bun_lock()
        self.change('analytics-core', 'fix: event')
        self.assertEqual(self.selection(), {'analytics-core': '0.1.0', 'react-analytics': '0.0.10'})
        self.assertEqual(json.loads(self.read(path))['dependencies'], {'@temps-sdk/analytics-core': 'workspace:^0.1.0'})
        lock = self.read('sdks/node/bun.lock')
        self.write_bun_lock()
        self.assertEqual(self.read('sdks/node/bun.lock'), lock)
        self.assertIn('"@temps-sdk/analytics-core": "workspace:^0.1.0",', lock)

    def test_go_version_qualified_replace_applies_only_to_its_version(self):
        self.add('go-core', 'go', '1.0.0', published={'1.0.0'})
        self.add('go-app', 'go', '1.0.0', published={'1.0.0'})
        core, app = self.catalog['go-core']['name'], self.catalog['go-app']['name']
        go_mod = 'sdks/go/go-app/go.mod'
        for key in ('go-core', 'go-app'):
            self.release_tag(key, '1.0.0')
        self.change('go-core', 'fix: core')
        self.write(go_mod, f'module {app}\n\nrequire {core} v1.0.1\n\nreplace {core} v1.0.0 => ../go-core\n')
        self.assertEqual(self.selection(), {'go-core': '1.0.1'})
        self.write(go_mod, f'module {app}\n\nrequire {core} v1.0.0 // local\n\nreplace {core} v1.0.0 => ../go-core\n')
        self.write('.github/releases/prepared.json', '{}\n')
        self.write('sdks/go/go-core/VERSION', '1.0.0\n')
        self.assertEqual(self.selection(), {'go-app': '1.0.1', 'go-core': '1.0.1'})
        self.assertIn(f'require {core} v1.0.1 // local\n\nreplace {core} v1.0.1 => ../go-core\n', self.read(go_mod))

    def test_pypi_registry_versions_compare_by_pep_440(self):
        for published, message in [({'1.2'}, 'already published'), ({'1.2.0.post1'}, 'already published'),
                                   ({'1.2.1.post1'}, 'registry already has 1.2.1')]:
            with self.subTest(published=published):
                self.setUp()
                self.add('py', 'pypi', '1.1.0', name='temps-sample', published={'1.1.0', *published})
                self.release_tag('py', '1.1.0')
                self.change('py', 'feat: new')
                with self.assertRaisesRegex(ValueError, message):
                    self.prepare()
        self.setUp()
        self.add('py', 'pypi', '1.1.0', name='temps-sample', published={'1.1.0', '1.2.0.dev3', '1.2.0b1.dev1'})
        self.release_tag('py', '1.1.0')
        self.change('py', 'feat: new')
        self.assertEqual(self.selection('v9.0.0-beta.1'), {'py': '1.2.0b1'})

    def test_manifest_edit_must_change_the_version_field(self):
        self.add('py', 'pypi', '1.0.0', name='temps-sample', published={'1.0.0'})
        self.write('sdks/py/pyproject.toml', "[project]\nname = \"temps-sample\"\ndescription = \"\"\"\n"
                   "version = \"1.0.0\"\n\"\"\"\nversion = '1.0.0'\n")
        self.release_tag('py', '1.0.0')
        self.change('py', 'fix: bug')
        with self.assertRaisesRegex(ValueError, 'no literal version = "1.0.0" in \\[project\\]'):
            self.prepare()

    def test_failed_write_or_validation_restores_every_file(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('temps-plugin-protocol', 'crate', '1.0.0', published={'1.0.0'})
        for key in ('analytics-core', 'temps-plugin-protocol'):
            self.release_tag(key, self.version(key))
        self.change('analytics-core', 'fix: event')
        self.history.commit('fix(protocol): frame', ['crates/temps-plugin-protocol/src/lib.rs'])
        paths = ['.github/releases/prepared.json', 'Cargo.lock', 'sdks/node/bun.lock',
                 'sdks/node/packages/analytics-core/package.json', 'crates/temps-plugin-protocol/Cargo.toml']
        before = {path: self.read(path) for path in paths}
        real_write = Path.write_text
        writes = []
        def flaky(target, text, *args, **kwargs):
            writes.append(target)
            if len(writes) == 3:
                raise PermissionError(f'{target}: read-only')
            return real_write(target, text, *args, **kwargs)
        with patch.object(Path, 'write_text', flaky), self.assertRaises(PermissionError):
            self.prepare()
        self.assertEqual({path: self.read(path) for path in paths}, before)
        with patch.object(release, 'validate', side_effect=ValueError('validation failed')), \
                self.assertRaisesRegex(ValueError, 'validation failed'):
            self.prepare()
        self.assertEqual({path: self.read(path) for path in paths}, before)
        # Once the cause is gone a retry succeeds from the untouched inputs.
        self.assertEqual(self.selection(), {'analytics-core': '0.0.7', 'temps-plugin-protocol': '1.0.1'})

    def test_python_requirement_with_extras_is_repinned_and_keeps_them(self):
        self.add('py-core', 'pypi', '1.0.0', name='temps-core', published={'1.0.0'})
        self.add('py-app', 'pypi', '1.0.0', name='temps-app', published={'1.0.0'})
        self.write('sdks/py-app/pyproject.toml', '[project]\nname = "temps-app"\nversion = "1.0.0"\n'
                   'dependencies = ["temps-core[fast] == 1.0.0; python_version >= \'3.10\'"]\n\n'
                   '[tool.uv.sources]\ntemps-core = { path = "../py-core" }\n')
        self.write('sdks/py-app/uv.lock', 'version = 1\n\n[[package]]\nname = "temps-app"\nversion = "1.0.0"\n'
                   'source = { editable = "." }\n\n'
                   '[[package]]\nname = "temps-core"\nversion = "1.0.0"\nsource = { directory = "../py-core" }\n')
        for key in ('py-core', 'py-app'):
            self.release_tag(key, '1.0.0')
        self.change('py-core', 'fix: core')
        self.assertEqual(self.selection(), {'py-app': '1.0.1', 'py-core': '1.0.1'})
        self.assertIn('"temps-core[fast] == 1.0.1; python_version >= \'3.10\'"', self.read('sdks/py-app/pyproject.toml'))

    def test_rollback_restores_the_original_bytes_and_line_endings(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.release_tag('analytics-core', '0.0.6')
        manifest = self.root / 'sdks/node/packages/analytics-core/package.json'
        original = b'{\r\n  "name": "@temps-sdk/analytics-core",\r\n  "version": "0.0.6"\r\n}\r\n'
        manifest.write_bytes(original)
        self.change('analytics-core', 'fix: event')
        with patch.object(release, 'validate', side_effect=ValueError('validation failed')), \
                self.assertRaisesRegex(ValueError, 'validation failed'):
            self.prepare()
        self.assertEqual(manifest.read_bytes(), original)

    def prepare_and_tag(self, tag='v9.0.0'):
        """Merge the prep change and tag the server release at it, as a release cut does."""
        report, files = self.prepare(tag)
        self.history.commit(f'chore(release): prepare SDKs for {tag}', {path: self.read(path) for path in files})
        self.history.tag(tag)
        return report

    def verify(self, tag='v9.0.0'):
        return sdk_prepare.verify_tag(tag, self.root, history=self.history,
                                      published=lambda package: self.registry.get(package['name'], set()))

    def test_tagged_selection_is_re_derived_before_publishing(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('kv', 'npm', '1.0.0', published={'1.0.0'})
        for key in ('analytics-core', 'kv'):
            self.release_tag(key, self.version(key))
        self.change('analytics-core', 'fix: event')
        self.prepare_and_tag()
        self.assertEqual(self.verify(), ([], []))
        # The tag must be the checked-out commit, and re-deriving never writes.
        with self.assertRaisesRegex(ValueError, 'v9.1.0 is not the checked-out commit'):
            sdk_prepare.prepare('v9.1.0', self.root, history=self.history, published=lambda p: set(),
                                write=False, at_tag=True)
        with self.assertRaisesRegex(ValueError, 'never writes'):
            sdk_prepare.prepare('v9.0.0', self.root, history=self.history, published=lambda p: set(), at_tag=True)
        # A breaking change merged after the prep PR needs 0.1.0, not the prepared 0.0.7.
        self.change('analytics-core', 'feat!: drop the v1 events')
        self.history.tag('v9.0.0')
        files, released = self.verify()
        self.assertEqual(released, [])
        self.assertIn('sdks/node/packages/analytics-core/package.json', files)
        self.assertIn('.github/releases/prepared.json', files)

    def test_tagged_selection_with_a_late_unprepared_package_is_stale(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('kv', 'npm', '1.0.0', published={'1.0.0'})
        for key in ('analytics-core', 'kv'):
            self.release_tag(key, self.version(key))
        self.change('analytics-core', 'fix: event')
        self.prepare_and_tag()
        self.change('kv', 'fix: scan')
        self.history.tag('v9.0.0')
        files, _ = self.verify()
        self.assertIn('sdks/node/packages/kv/package.json', files)

    def test_tagged_selection_already_partly_released_is_still_verified(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.add('kv', 'npm', '1.0.0', published={'1.0.0'})
        for key in ('analytics-core', 'kv'):
            self.release_tag(key, self.version(key))
        self.change('analytics-core', 'fix: event')
        self.change('kv', 'fix: scan')
        self.prepare_and_tag()
        # A re-run after analytics-core was published and tagged still re-derives the
        # whole selection, as if this release had published nothing yet.
        self.registry['@temps-sdk/analytics-core'].add('0.0.7')
        self.release_tag('analytics-core', '0.0.7')
        self.history.tag('v9.0.0')  # the package tag moved HEAD's ref map; keep the server tag at HEAD
        self.assertEqual(self.verify(), ([], ['analytics-core']))
        # ...so a late change to a package not yet published is still caught.
        self.change('kv', 'feat!: drop the v1 client')
        self.history.tag('v9.0.0')
        files, released = self.verify()
        self.assertEqual(released, ['analytics-core'])
        self.assertIn('sdks/node/packages/kv/package.json', files)

    def test_an_older_tag_is_verified_without_the_sdk_releases_that_followed_it(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.release_tag('analytics-core', '0.0.6')
        self.change('analytics-core', 'fix: event')
        self.prepare_and_tag()
        self.registry['@temps-sdk/analytics-core'].add('0.0.7')
        self.release_tag('analytics-core', '0.0.7')
        # A later server release published 0.0.8 from a commit after this tag.
        self.registry['@temps-sdk/analytics-core'].add('0.0.8')
        self.release_tag('analytics-core', '0.0.8')
        self.history.unreachable.add('sdk/analytics-core/v0.0.8')
        self.assertEqual(self.verify(), ([], ['analytics-core']))

    def test_packages_sharing_a_name_on_different_registries_are_verified_separately(self):
        # The Python and Rust plugin SDKs are both `temps-plugin-sdk`, on PyPI and crates.io.
        self.add('python-plugin', 'pypi', '1.0.0', name='temps-plugin-sdk', path='sdks/python-plugin')
        self.add('temps-plugin-sdk', 'crate', '2.0.0', name='temps-plugin-sdk')
        registry = {'pypi': {'1.0.0'}, 'crate': {'2.0.0'}}
        published = lambda package: registry[package['kind']]
        for key in ('python-plugin', 'temps-plugin-sdk'):
            self.release_tag(key, self.version(key))
        self.history.commit('fix(plugin): python', ['sdks/python-plugin/plugin.py'])
        self.history.commit('fix(plugin): rust', ['crates/temps-plugin-sdk/src/lib.rs'])
        report, files = sdk_prepare.prepare('v9.0.0', self.root, history=self.history, published=published)
        self.assertEqual({e['key']: e['version'] for e in report}, {'python-plugin': '1.0.1', 'temps-plugin-sdk': '2.0.1'})
        self.history.commit('chore(release): prepare SDKs for v9.0.0', {path: self.read(path) for path in files})
        self.history.tag('v9.0.0')
        # Only the Python package was published before the run stopped.
        registry['pypi'].add('1.0.1')
        self.release_tag('python-plugin', '1.0.1')
        self.assertEqual(sdk_prepare.verify_tag('v9.0.0', self.root, history=self.history, published=published),
                         ([], ['python-plugin']))

    def test_server_only_tag_has_nothing_to_verify(self):
        self.add('analytics-core', 'npm', '0.0.6', published={'0.0.6'})
        self.release_tag('analytics-core', '0.0.6')
        self.change('analytics-core', 'fix: event')
        self.history.tag('v9.0.0')
        self.assertEqual(self.verify(), ([], []))

    def test_commit_touching_no_package_selects_nothing(self):
        self.add('kv', 'npm', '1.0.0', published={'1.0.0'})
        self.add('py', 'pypi', '1.0.0', name='temps-sample', published={'1.0.0'})
        self.release_tag('kv', '1.0.0')
        self.release_tag('py', '1.0.0')
        self.history.commit('feat(web): new console page', ['web/src/page.tsx', 'crates/temps-core/src/lib.rs'])
        self.history.commit('fix(kv)!: only the subject names kv', ['docs/kv.md'])
        self.assertEqual(self.prepare(), ([], []))

    def test_nested_catalog_paths_belong_to_the_inner_package_only(self):
        self.add('python-plugin', 'pypi', '1.0.0', name='temps-plugin-sdk', path='sdks/python', published={'1.0.0'})
        self.add('python-workflows', 'pypi', '1.0.0', name='temps-workflows', path='sdks/python/workflows',
                 published={'1.0.0'})
        self.release_tag('python-plugin', '1.0.0')
        self.release_tag('python-workflows', '1.0.0')
        self.history.commit('feat(workflows): retries', ['sdks/python/workflows/temps_workflows/run.py'])
        self.assertEqual(self.selection(), {'python-workflows': '1.1.0'})

    def test_declared_sources_count_as_package_changes(self):
        self.add('api', 'npm', '1.0.0', published={'1.0.0'}, sources=['apps/temps-cli/openapi.json'])
        self.release_tag('api', '1.0.0')
        self.history.commit('feat(flags): new endpoint', ['apps/temps-cli/openapi.json'])
        self.assertEqual(self.selection(), {'api': '1.1.0'})

    def test_manual_unpublished_bump_ahead_of_the_commits_is_kept(self):
        self.add('kv', 'npm', '1.2.3', published={'1.2.3'})
        self.release_tag('kv', '1.2.3')
        path = 'sdks/node/packages/kv/package.json'
        self.history.commit('chore(kv): prepare 2.0', {path: self.write(
            path, json.dumps({'name': '@temps-sdk/kv', 'version': '2.0.0'}) + '\n')})
        self.write_bun_lock()
        self.change('kv', 'fix: patch')
        self.assertEqual(self.selection(), {'kv': '2.0.0'})

    def test_go_major_two_requires_a_new_module_path(self):
        self.add('workflows', 'go', '1.4.0', published={'1.4.0'})
        self.release_tag('workflows', '1.4.0')
        self.change('workflows', 'feat!: new API')
        with self.assertRaisesRegex(ValueError, 'Go v2\\+ requires a new /v2 module path'):
            self.prepare()

    def test_target_tag_rules(self):
        self.add('kv', 'npm', '1.0.0')
        for tag, message in [('v9.0.0-nightly.20261119.abcdef', 'Nightly'), ('sdk/kv/v1.0.0', 'Not a server'),
                             ('v9.0', 'Not a server'), ('v9.0.0;rm', 'Not a server')]:
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, message):
                self.prepare(tag)
        self.history.refs['v9.0.0'] = self.history.log[-1]['sha']
        with self.assertRaisesRegex(ValueError, 'already tagged'):
            self.prepare('v9.0.0')

    def test_registry_lookups_cover_every_ever_used_version(self):
        def fetch(url, **kwargs):
            pages = {
                'https://registry.npmjs.org/%40temps-sdk%2Fkv': {
                    'versions': {'1.0.0': {}}, 'time': {'created': '', 'modified': '', '1.0.0': '', '1.0.1': ''}},
                'https://pypi.org/pypi/temps-sample/json': {'releases': {'1.0.0b1': []}},
                'https://crates.io/api/v1/crates/temps-sample/versions?per_page=100': {
                    'versions': [{'num': '1.0.0', 'yanked': True}], 'meta': {'next_page': '?per_page=100&seek=x'}},
                'https://crates.io/api/v1/crates/temps-sample/versions?per_page=100&seek=x': {
                    'versions': [{'num': '0.9.0', 'yanked': False}], 'meta': {'next_page': None}},
            }
            if 'proxy.golang.org' in url:
                self.assertEqual(url, 'https://proxy.golang.org/github.com/!example/mod/@v/list')
                self.assertEqual(kwargs['missing'], (404, 410))
                return kwargs['parse'](io.BytesIO(b'v1.0.0\nv1.1.0-beta.1\n'))
            return pages.get(url)
        versions = sdk_prepare.published_versions
        self.assertEqual(versions({'kind': 'npm', 'name': '@temps-sdk/kv'}, fetch), {'1.0.0', '1.0.1'})
        self.assertEqual(versions({'kind': 'npm', 'name': '@temps-sdk/new'}, fetch), set())
        self.assertEqual(versions({'kind': 'pypi', 'name': 'temps-sample'}, fetch), {'1.0.0b1'})
        self.assertEqual(versions({'kind': 'crate', 'name': 'temps-sample'}, fetch), {'1.0.0', '0.9.0'})
        self.assertEqual(versions({'kind': 'go', 'name': 'github.com/Example/mod'}, fetch), {'1.0.0', '1.1.0-beta.1'})

    def test_summary_lists_each_package_without_mentions(self):
        self.add('kv', 'npm', '1.0.0', published={'1.0.0'})
        self.release_tag('kv', '1.0.0')
        self.change('kv', 'fix(kv): thanks @someone for `scan`')
        report, _ = self.prepare('v9.0.0-beta.1')
        text = sdk_prepare.summary('v9.0.0-beta.1', report)
        self.assertIn('| `@temps-sdk/kv` | npm | `1.0.0` | `1.0.1-beta.1` | 1 commit (patch) |', text)
        self.assertIn("`fix(kv): thanks @someone for 'scan'`", text)
        self.assertIn('nothing to prepare', sdk_prepare.summary('v9.0.0', []))


class GitHistoryTests(unittest.TestCase):
    """The real git adapter, against a throwaway repository."""

    def git(self, *args):
        return subprocess.run(('git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
                               '-c', 'commit.gpgsign=false', '-c', 'tag.gpgsign=false', *args),
                              cwd=self.root, check=True, text=True, capture_output=True).stdout

    def commit(self, path, content, message):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content)
        self.git('add', path)
        self.git('commit', '-q', '-m', message)
        return self.git('rev-parse', 'HEAD').strip()

    def test_tags_commits_and_manifest_revisions(self):
        if shutil.which('git') is None:
            self.skipTest('git is not installed')
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.git('init', '-q', '-b', 'main')
        first = self.commit('sdks/python/pyproject.toml', '[project]\nversion = "1.0.0"\n', 'chore: add')
        self.git('tag', '-a', 'sdk/python/v1.0.0', '-m', 'release')
        self.commit('sdks/python/workflows/run.py', 'x', 'feat(workflows): nested')
        self.commit('sdks/python/plugin.py', 'x', 'fix(plugin): outer\n\nBREAKING CHANGE: renamed')
        history = sdk_prepare.GitHistory(self.root)
        history.ensure_full_history()
        self.assertEqual(history.tags('sdk/python/v'), ['sdk/python/v1.0.0'])
        self.assertTrue(history.tag_exists('sdk/python/v1.0.0'))
        self.assertFalse(history.tag_exists('v1.0.0'))
        commits = history.commits('refs/tags/sdk/python/v1.0.0', ['sdks/python'], ['sdks/python/workflows'])
        self.assertEqual([c['subject'] for c in commits], ['fix(plugin): outer'])
        self.assertIn('BREAKING CHANGE: renamed', commits[0]['body'])
        self.assertEqual(history.revisions('sdks/python/pyproject.toml'), [first])
        self.git('switch', '-q', '-c', 'later')
        self.commit('sdks/python/plugin.py', 'y', 'fix(plugin): later')
        self.git('tag', 'sdk/python/v1.0.1')
        self.git('switch', '-q', 'main')
        self.assertEqual(history.reachable_tags(), {'sdk/python/v1.0.0'})
        self.assertIn('1.0.0', history.show(first, 'sdks/python/pyproject.toml'))
        self.assertIsNone(history.show(first, 'missing.toml'))


    def release_branch_repo(self):
        if shutil.which('git') is None:
            self.skipTest('git is not installed')
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.git('init', '-q', '-b', 'main')
        self.catalog = {'kv': {'kind': 'npm', 'path': 'sdks/node/packages/kv', 'name': '@temps-sdk/kv'}}
        self.commit('.github/releases/packages.json', json.dumps(self.catalog), 'chore: catalog')
        self.commit('sdks/node/packages/kv/package.json', '{"name": "@temps-sdk/kv", "version": "1.0.0"}', 'chore: add kv')
        self.git('switch', '-q', '-c', 'release/v9.0.0')
        self.commit('Cargo.toml', 'version = "9.0.0"\n', 'chore: bump version to v9.0.0')
        self.git('switch', '-q', 'main')
        self.commit('sdks/node/packages/kv/src/index.ts', 'x', 'fix(kv): on main')
        return sdk_prepare.GitHistory(self.root)

    def branch_changes(self, history):
        self.git('switch', '-q', 'main')
        return sdk_prepare.branch_only_changes('release/v9.0.0', self.root, history=history)

    def test_a_bump_only_release_branch_has_no_branch_only_sdk_changes(self):
        history = self.release_branch_repo()
        # The server bump, and SDK commits already on main, are fine.
        self.assertEqual(self.branch_changes(history), [])
        with self.assertRaisesRegex(ValueError, 'release/v9.9.9 does not name a commit'):
            sdk_prepare.branch_only_changes('release/v9.9.9', self.root, history=history)

    def test_sdk_commits_only_on_the_release_branch_are_found(self):
        history = self.release_branch_repo()
        self.git('switch', '-q', 'release/v9.0.0')
        self.commit('sdks/node/packages/kv/src/scan.ts', 'x', 'fix(kv): only on the branch')
        self.assertEqual(self.branch_changes(history), [('kv', ['sdks/node/packages/kv/src/scan.ts'])])

    def test_sdk_changes_made_in_a_release_branch_merge_are_found(self):
        history = self.release_branch_repo()
        self.git('switch', '-q', 'release/v9.0.0')
        self.git('merge', '-q', '--no-ff', '--no-commit', 'main')
        (self.root / 'sdks/node/packages/kv/src/index.ts').write_text('edited while merging')
        self.git('add', '-A')
        self.git('commit', '-q', '-m', "Merge branch 'main' into release/v9.0.0")
        self.assertEqual(self.branch_changes(history), [('kv', ['sdks/node/packages/kv/src/index.ts'])])

    def test_a_branch_fix_also_cherry_picked_onto_main_is_not_reported(self):
        history = self.release_branch_repo()
        self.git('switch', '-q', 'release/v9.0.0')
        fix = self.commit('sdks/node/packages/kv/src/scan.ts', 'fixed', 'fix(kv): on the branch first')
        self.git('switch', '-q', 'main')
        self.git('cherry-pick', fix)
        self.assertEqual(self.branch_changes(history), [])
        # Content that differs again on the branch is reported.
        self.git('switch', '-q', 'release/v9.0.0')
        self.commit('sdks/node/packages/kv/src/scan.ts', 'fixed differently', 'fix(kv): branch only')
        self.assertEqual(self.branch_changes(history), [('kv', ['sdks/node/packages/kv/src/scan.ts'])])

    def test_a_branch_rename_reports_its_source_unless_main_made_the_same_rename(self):
        history = self.release_branch_repo()
        old, new = 'sdks/node/packages/kv/src/old.ts', 'sdks/node/packages/kv/src/new.ts'
        self.commit(old, 'client', 'feat(kv): client')
        self.git('switch', '-q', 'release/v9.0.0')
        self.git('merge', '-q', '--no-edit', 'main')
        self.git('mv', old, new)
        self.git('commit', '-q', '-m', 'refactor(kv): rename on the branch')
        rename = self.git('rev-parse', 'HEAD').strip()
        self.git('switch', '-q', 'main')
        # main copies the file instead: the branch's deletion of the source is branch-only.
        self.commit(new, 'client', 'chore(kv): copy')
        self.assertEqual(self.branch_changes(history), [('kv', [old])])
        # The same rename cherry-picked onto main leaves nothing branch-only.
        self.git('rm', '-q', new)
        self.git('commit', '-q', '-m', 'chore(kv): undo the copy')
        self.git('cherry-pick', rename)
        self.assertEqual(self.branch_changes(history), [])

    def test_branch_only_file_names_with_spaces_are_kept_whole(self):
        history = self.release_branch_repo()
        self.git('switch', '-q', 'release/v9.0.0')
        self.commit('sdks/node/packages/kv/src/legacy client [v1].ts', 'x', 'fix(kv): legacy client')
        self.assertEqual(self.branch_changes(history), [('kv', ['sdks/node/packages/kv/src/legacy client [v1].ts'])])

    def test_packages_and_release_metadata_added_only_on_the_branch_are_found(self):
        history = self.release_branch_repo()
        self.git('switch', '-q', 'release/v9.0.0')
        catalog = dict(self.catalog, blob={'kind': 'npm', 'path': 'sdks/node/packages/blob', 'name': '@temps-sdk/blob'})
        self.commit('sdks/node/packages/blob/package.json', '{"name": "@temps-sdk/blob", "version": "0.1.0"}', 'feat(blob): add')
        self.commit('.github/releases/packages.json', json.dumps(catalog), 'chore: catalog blob')
        self.assertEqual(self.branch_changes(history), [
            ('blob', ['sdks/node/packages/blob/package.json']),
            ('release metadata', ['.github/releases/packages.json'])])


class PrepareCommandTests(unittest.TestCase):
    """Exit statuses: 0 prepared, 1 files would change, 2 the check itself failed."""

    def run_command(self, *argv):
        parser = argparse.ArgumentParser()
        args = argparse.Namespace(tag='v9.0.0', check='--check' in argv, at_tag='--at-tag' in argv,
                                  candidate='release/v9.0.0' if '--candidate' in argv else None,
                                  summary=None, changed_files=None)
        with patch.dict(os.environ), redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            os.environ.pop('GITHUB_OUTPUT', None)
            try:
                release.prepare_command(args, parser)
            except SystemExit as exit:
                return exit.code, err.getvalue()
        return 0, err.getvalue()

    def test_pending_edits_exit_1_and_failed_checks_exit_2(self):
        with patch.object(sdk_prepare, 'prepare', return_value=([], [])):
            self.assertEqual(self.run_command('--check')[0], 0)
        with patch.object(sdk_prepare, 'prepare', return_value=([], ['.github/releases/prepared.json'])):
            self.assertEqual(self.run_command('--check')[0], 1)
        for error in (ValueError('kv: bad manifest'), urllib.error.URLError('registry unreachable'),
                      TimeoutError('timed out'), KeyError('version')):
            with self.subTest(error=error), patch.object(sdk_prepare, 'prepare', side_effect=error):
                status, message = self.run_command('--check')
                self.assertEqual(status, 2)
                self.assertIn('error:', message)

    def test_release_branch_commits_missing_from_main_fail_the_check(self):
        found = [('kv', ['sdks/node/packages/kv/src/scan.ts'])]
        with patch.object(sdk_prepare, 'branch_only_changes', return_value=found), \
                patch.object(sdk_prepare, 'prepare') as prepare:
            status, message = self.run_command('--candidate')
        self.assertEqual(status, 2)
        self.assertIn('kv: sdks/node/packages/kv/src/scan.ts', message)
        prepare.assert_not_called()

    def test_a_failure_writing_outputs_exits_2_not_1(self):
        with patch.object(sdk_prepare, 'prepare', return_value=([], [])), \
                patch.object(sdk_prepare, 'summary', side_effect=OSError('disk full')):
            status, message = self.run_command('--check')
        self.assertEqual(status, 2)
        self.assertIn('OSError: disk full', message)

    def test_at_tag_fails_only_on_a_stale_selection(self):
        for result, expected in [(([], []), 0), (([], ['kv']), 0), ((['sdks/node/packages/kv/package.json'], []), 1)]:
            with self.subTest(result=result), patch.object(sdk_prepare, 'verify_tag', return_value=result):
                self.assertEqual(self.run_command('--check', '--at-tag')[0], expected)
        with patch.object(sdk_prepare, 'verify_tag', side_effect=urllib.error.URLError('down')):
            self.assertEqual(self.run_command('--check', '--at-tag')[0], 2)
        self.assertEqual(self.run_command('--at-tag')[0], 2)  # re-deriving at a tag is check-only


class SdkReleasePrepWorkflowTests(unittest.TestCase):
    workflow = (release.ROOT / '.github/workflows/sdk-release-prep.yml').read_text()

    def test_prep_holds_no_registry_credentials(self):
        self.assertNotIn('id-token', self.workflow)
        self.assertNotIn('environment:', self.workflow)
        self.assertEqual(re.findall(r'secrets\.\w+', self.workflow), [])
        for forbidden in ('NPM_TOKEN', 'NODE_AUTH_TOKEN', 'CARGO_REGISTRY', 'PYPI', 'publish --'):
            self.assertNotIn(forbidden, self.workflow)
        self.assertIn('\npermissions: {}\n', self.workflow)
        jobs = self.workflow.split('\njobs:\n')[1]
        blocks = dict(re.findall(r'^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)', jobs, re.M | re.S))
        self.assertEqual(sorted(blocks), ['prepare', 'pull-request'])
        def permissions(job):
            return re.search(r'^    permissions:\n((?:      \S.*\n)+)', blocks[job], re.M)[1]
        self.assertEqual(permissions('prepare'), '      contents: read\n')
        self.assertEqual(permissions('pull-request'), '      contents: write\n      pull-requests: write\n')
        # Only the job without write access builds third-party package code.
        self.assertIn('sdk_release.py build', blocks['prepare'])
        self.assertNotIn('sdk_release.py build', blocks['pull-request'])
        self.assertNotIn('bun install', blocks['pull-request'])

    def test_actions_are_pinned_by_sha(self):
        uses = re.findall(r'uses:\s*(\S+)', self.workflow)
        self.assertTrue(uses)
        for action in uses:
            self.assertRegex(action, r'^[\w.-]+/[\w./-]+@[0-9a-f]{40}$')

    def test_prepares_validates_and_dry_builds_before_opening_the_pr(self):
        steps = [self.workflow.index(text) for text in (
            'sdk_release.py prepare --tag', 'sdk_release.py validate', 'sdk_release.py build --tag',
            'gh pr create')]
        self.assertEqual(steps, sorted(steps))
        self.assertIn('chore(release): prepare SDKs for ${TARGET}', self.workflow)
        self.assertIn('git commit -q -s -F -', self.workflow)
        self.assertIn('release/v*', self.workflow)

    def test_release_branch_pushes_reject_sdk_commits_missing_from_main(self):
        prepare = self.workflow.index('name: Prepare SDK versions for the server tag')
        step = self.workflow[prepare:self.workflow.index('- name:', prepare + 1)]
        self.assertIn('candidate=(--candidate "refs/remotes/origin/${REF_NAME}")', step)
        self.assertIn('"${candidate[@]}"', step)

    def test_pr_body_asks_for_workflow_approval(self):
        self.assertIn('Approve workflows to run', self.workflow)
        self.assertNotIn('reopen', self.workflow)

    def test_release_run_re_derives_the_selection_before_anything_publishes(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        job = workflow[workflow.index('\n  validate-release-ref:\n'):workflow.index('\n  require-tests:\n')]
        self.assertIn('fetch-depth: 0', job)
        step = job[job.index('name: Verify the prepared SDK selection at the tag'):]
        self.assertIn('prepare --tag "$RELEASE_TAG" --check --at-tag', step)
        self.assertIn("github.ref_type == 'tag' && steps.sdk.outputs.selected == 'true'", step)
        self.assertNotIn('continue-on-error', step)
        # Every publishing job runs after validate-release-ref.
        self.assertIn('needs: [validate-release-ref, require-tests, build-web-assets, create-release]', workflow)

    def test_release_script_refuses_unprepared_sdks(self):
        script = (release.ROOT / 'scripts/release.sh').read_text()
        self.assertIn('python3 scripts/sdk_release.py prepare --tag "v${VERSION}" --check', script)
        self.assertLess(script.index('prepare --tag "v${VERSION}" --check'), script.index('git tag -a'))
        # A stale, non-empty selection stops the release instead of offering to continue.
        stale = script.index('python3 scripts/sdk_release.py plan --tag "v${VERSION}")" != "[]"')
        self.assertLess(script.index('exit 1', stale), script.index('read -r CONTINUE', stale))
        # Only pending edits (exit 1) offer the server-only release; a failed check aborts.
        failed = script.index('if [ "$PREPARE_STATUS" -ne 0 ] && [ "$PREPARE_STATUS" -ne 1 ]; then')
        self.assertLess(script.index('exit 1', failed), script.index('if [ "$PREPARE_STATUS" -eq 1 ]; then'))
        self.assertLess(script.index('if [ "$PREPARE_STATUS" -eq 1 ]; then'), script.index('read -r CONTINUE', stale))


if __name__ == '__main__':
    unittest.main()

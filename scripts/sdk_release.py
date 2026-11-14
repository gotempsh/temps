# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Explicit, independent SDK releases. Planning/build/check never publishes."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SEMVER = r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(alpha|beta|rc)\.(0|[1-9]\d*))?'
PYVER = r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:(a|b|rc)(0|[1-9]\d*))?'


def run(*args, cwd=ROOT, capture=False):
    return subprocess.run(args, cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None, timeout=1800).stdout


def read_json(path):
    return json.loads(Path(path).read_text())


def channel(version, kind='npm'):
    match = re.fullmatch(PYVER if kind == 'pypi' else SEMVER, version)
    if not match:
        raise ValueError(f'Unsupported {kind} release version: {version}')
    return 'beta' if match[4] else 'stable'


def manifest(package, root=ROOT):
    path = root / package['path']
    kind = package['kind']
    if kind == 'npm':
        return read_json(path / 'package.json')
    if kind == 'go':
        module = re.search(r'^module (\S+)$', (path / 'go.mod').read_text(), re.M)[1]
        return {'name': module, 'version': (path / 'VERSION').read_text().strip()}
    file, field = ('pyproject.toml', 'project') if kind == 'pypi' else ('Cargo.toml', 'package')
    return tomllib.loads((path / file).read_text())[field]


def package_tag(key, package, version):
    return f"{package['path']}/v{version}" if package['kind'] == 'go' else f'sdk/{key}/v{version}'


def catalog_packages(root=ROOT):
    catalog = read_json(root / '.github/releases/packages.json')
    packages = []
    for key, package in catalog.items():
        actual = manifest(package, root)
        if actual['name'] != package['name']:
            raise ValueError(f'{key}: catalog name disagrees with manifest')
        version = actual['version']
        packages.append(dict(package, key=key, version=version,
                             channel=channel(version, package['kind']),
                             tag=package_tag(key, package, version)))
    return sorted(packages, key=lambda p: (p['key'] not in ('analytics-core', 'temps-plugin-protocol'), p['key']))


def validate(root=ROOT):
    catalog_packages(root)
    for tag in read_json(root / '.github/releases/prepared.json'):
        plan(tag, root)


def plan(tag, root=ROOT):
    catalog = read_json(root / '.github/releases/packages.json')
    prepared = read_json(root / '.github/releases/prepared.json')
    if re.fullmatch(r'v\d+\.\d+\.\d+-nightly\.[\w.-]+', tag):
        if tag in prepared:
            raise ValueError('Nightly server releases cannot prepare SDK releases')
        return []
    elif re.fullmatch(r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?', tag):
        # Preserve the server's existing validate-release-ref.sh grammar.
        selected = prepared.get(tag, {})
        expected_channel = 'beta' if '-' in tag else 'stable'
    else:
        matches = [(key, p, tag.removeprefix(prefix)) for key, p in catalog.items()
                   if tag.startswith(prefix := package_tag(key, p, ''))]
        if len(matches) != 1:
            raise ValueError(f'Unknown SDK/server release tag: {tag}')
        key, package, version = matches[0]
        selected = {key: version}
        expected_channel = channel(version, package['kind'])
    result = []
    for key, version in selected.items():
        if key not in catalog:
            raise ValueError(f'Unknown prepared package: {key}')
        package = catalog[key]
        actual = manifest(package, root)
        if actual['name'] != package['name'] or actual['version'] != version:
            raise ValueError(f'{key}: prepared version/name disagrees with package manifest')
        if channel(version, package['kind']) != expected_channel:
            raise ValueError(f'{key}: package and release channels disagree')
        if package['kind'] == 'npm' and expected_channel == 'stable':
            siblings = {p['name']: p for p in catalog.values() if p['kind'] == 'npm'}
            for dependency, requirement in actual.get('dependencies', {}).items():
                if requirement.startswith('workspace:') and dependency in siblings:
                    sibling_version = manifest(siblings[dependency], root)['version']
                    if channel(sibling_version) != 'stable':
                        raise ValueError(f'{key}: stable package depends on prerelease sibling {dependency}')
        if package['kind'] == 'go' and int(version.split('.')[0]) >= 2:
            raise ValueError('Go v2+ requires a new /v2 module path and catalog entry')
        result.append(dict(package, key=key, version=version, channel=expected_channel,
                           tag=package_tag(key, package, version)))
    # Protocol before the Rust SDK, analytics core before its consumers.
    priority = {'temps-plugin-protocol': 0, 'analytics-core': 0}
    return sorted(result, key=lambda p: (priority.get(p['key'], 1), p['key']))


def notes(packages):
    lines = ['\n## Prepared SDK package versions\n',
             'Publication is tracked by the Publish prepared SDK releases job; this table records intent, not registry success.\n',
             '| Package | Version | Channel | Package tag |', '| --- | --- | --- | --- |']
    lines += [f"| `{p['name']}` | `{p['version']}` | {p['channel']} | `{p['tag']}` |" for p in packages]
    return '\n'.join(lines) + '\n' if packages else '\nNo SDK releases are prepared for this server tag.\n'


def check_npm_archive(path, package):
    with tarfile.open(path) as archive:
        packed = json.load(archive.extractfile('package/package.json'))
        names = archive.getnames()
        if not any(n.startswith('package/dist/') for n in names):
            raise ValueError(f"{package['key']}: package has no built dist files")
        if (packed['name'], packed['version']) != (package['name'], package['version']):
            raise ValueError('Packed npm manifest does not match release')
        entries = packed.get('exports', {'.': packed.get('main')})
        def targets(entry):
            if isinstance(entry, str):
                return [entry]
            if isinstance(entry, dict):
                return [target for value in entry.values() for target in targets(value)]
            return []
        for target in targets(entries) + targets(packed.get('bin', {})) + targets(packed.get('types')):
            if '*' not in target and 'package/' + target.removeprefix('./') not in names:
                raise ValueError(f"Packed entrypoint is missing: {target}")
        for field in ('dependencies', 'peerDependencies', 'optionalDependencies'):
            if any(v.startswith(('workspace:', 'file:', 'link:')) for v in packed.get(field, {}).values()):
                raise ValueError('Packed npm manifest contains local dependency ranges')


def build(packages, output):
    output.mkdir(parents=True, exist_ok=True)
    os.environ['SOURCE_DATE_EPOCH'] = run('git', 'show', '-s', '--format=%ct', 'HEAD', capture=True).strip()
    installed = set()
    crates = [p for p in packages if p['kind'] == 'crate']
    if crates:
        selection = [arg for p in crates for arg in ('-p', p['name'])]
        run('cargo', 'check', '--lib', '--locked', *selection)
        run('cargo', 'test', '--lib', '--locked', *selection)
        # Package interdependent crates together, including the unpublished protocol.
        run('cargo', 'package', '--locked', *selection)
    for p in packages:
        path = ROOT / p['path']
        dest = output / p['key']
        dest.mkdir(exist_ok=True)
        if any(dest.iterdir()):
            raise ValueError(f'Artifact directory must be empty: {dest}')
        if p['kind'] == 'npm':
            install = ROOT / 'sdks/node' if p['path'].startswith('sdks/node/') else path
            if install not in installed:
                run('bun', 'install', '--frozen-lockfile', cwd=install)
                installed.add(install)
            data = manifest(p)
            # Workspace consumers require core declarations before their build.
            if '@temps-sdk/analytics-core' in data.get('dependencies', {}):
                run('bun', 'run', 'build', cwd=ROOT / 'sdks/node/packages/analytics-core')
            if 'test:run' in data['scripts']:
                run('bun', 'run', 'test:run', cwd=path)
            elif data['scripts'].get('test') == 'bun test':
                run('bun', 'test', cwd=path)
            elif data['scripts'].get('test') == 'vitest':
                run('bun', 'x', '--no-install', 'vitest', 'run', cwd=path)
            run('bun', 'run', 'build', cwd=path)
            if p['key'] in ('node-sdk', 'api', 'workflows', 'kv', 'blob', 'sandbox', 'plugin'):
                run('node', str(ROOT / 'scripts/sdk-package.test.mjs'), str(path))
            artifact = dest / 'package.tgz'
            run('bun', 'pm', 'pack', '--ignore-scripts', '--filename', str(artifact), cwd=path)
            check_npm_archive(artifact, p)
            run('npm', 'pack', str(artifact), '--dry-run', '--ignore-scripts', cwd=dest)
        elif p['kind'] == 'pypi':
            run('uv', 'run', '--no-project', '--with', 'build', 'python', '-m', 'build', str(path), '--outdir', str(dest))
            run('uv', 'run', '--no-project', '--with', 'twine', 'twine', 'check', *map(str, dest.iterdir()))
            wheel = next(dest.glob('*.whl'))
            # Run against the installed wheel from outside the source checkout.
            tests = path / ('temps_workflows' if p['key'] == 'python-workflows' else 'tests')
            command = (['temps_workflows.test_runtime']
                       if p['key'] == 'python-workflows' else ['discover', '-s', str(tests), '-p', 'test_*.py'])
            run('uv', 'run', '--no-project', '--with', str(wheel), 'python', '-m', 'unittest', *command, cwd=dest)
        elif p['kind'] == 'crate':
            metadata = json.loads(run('cargo', 'metadata', '--no-deps', '--format-version', '1', capture=True))
            shutil.copy(Path(metadata['target_directory']) / 'package' / f"{p['name']}-{p['version']}.crate", dest)
        else:
            run('go', 'test', '-race', './...', cwd=path)
            run('go', 'vet', './...', cwd=path)
            run('git', 'archive', '--format=tar.gz', f'--output={dest / "module.tar.gz"}', 'HEAD', p['path'])
    (output / 'plan.json').write_text(json.dumps(packages, indent=2) + '\n')
    (output / 'revision').write_text(run('git', 'rev-parse', 'HEAD', capture=True))
    inventory = {str(f.relative_to(output)): digest(f) for p in packages
                 for f in (output / p['key']).iterdir()}
    (output / 'sha256.json').write_text(json.dumps(inventory, sort_keys=True) + '\n')


def registry_json(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'temps-sdk-release (https://github.com/gotempsh/temps)'})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code == 404:
                return None
            if error.code not in (429, 500, 502, 503, 504) or attempt == 2:
                raise
        except (urllib.error.URLError, TimeoutError):
            if attempt == 2:
                raise
        time.sleep(2 ** attempt)


def version_order(version):
    match = re.fullmatch(SEMVER, version)
    if not match:
        raise ValueError(f'Cannot order registry version: {version}')
    rank = {"alpha": 0, "beta": 1, "rc": 2, None: 3}
    return (*map(int, match.group(1, 2, 3)), rank[match[4]], int(match[5] or 0))


def guard_npm_channel(package, fetch=registry_json):
    alias = 'beta' if package['channel'] == 'beta' else 'latest'
    name = urllib.parse.quote(package['name'], safe='')
    current = fetch(f'https://registry.npmjs.org/{name}/{alias}')
    if current and version_order(current['version']) > version_order(package['version']):
        raise ValueError(f"{package['name']}: refusing to move {alias} backwards from {current['version']}")


def digest(path, algorithm='sha256'):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def pending_files(package, directory, fetch=registry_json):
    """Skip only byte-identical existing files; fail closed on conflicting versions."""
    name, version, kind = package['name'], package['version'], package['kind']
    files = list(directory.iterdir())
    expected = {'npm': ['.tgz'], 'crate': ['.crate'], 'pypi': ['.tar.gz', '.whl']}[kind]
    if (len(files) != len(expected)
            or any(sum(f.name.endswith(suffix) for f in files) != 1 for suffix in expected)
            or any(not f.is_file() or f.is_symlink() for f in files)):
        raise ValueError(f'{name}@{version}: incomplete or unexpected package artifacts')
    encoded = urllib.parse.quote(name, safe='')
    if kind == 'npm':
        metadata = fetch(f'https://registry.npmjs.org/{encoded}/{version}')
        files = [directory / 'package.tgz']
        if metadata is None:
            return files
        integrity = 'sha512-' + base64.b64encode(bytes.fromhex(digest(files[0], 'sha512'))).decode()
        if metadata['dist']['integrity'] != integrity:
            raise ValueError(f'{name}@{version}: published npm bytes differ; prepare a new version')
        return []
    if kind == 'crate':
        metadata = fetch(f'https://crates.io/api/v1/crates/{encoded}/{version}')
        files = list(directory.glob('*.crate'))
        if metadata is None:
            return files
        if metadata['version']['checksum'] != digest(files[0]) or metadata['version']['yanked']:
            raise ValueError(f'{name}@{version}: conflicting or yanked crate')
        return []
    metadata = fetch(f'https://pypi.org/pypi/{encoded}/{version}/json')
    existing = {f['filename']: f for f in metadata['urls']} if metadata else {}
    pending = []
    for file in sorted(directory.iterdir()):
        published = existing.get(file.name)
        if published is None:
            pending.append(file)
        elif published['digests']['sha256'] != digest(file) or published.get('yanked'):
            raise ValueError(f'{name}@{version}: conflicting or yanked PyPI file {file.name}')
    return pending


def check_dependencies(packages, kind, fetch=registry_json):
    catalog = {p['name']: p for p in catalog_packages() if p['kind'] == kind}
    selected = {p['name'] for p in packages if p['kind'] == kind}
    for p in packages:
        if p['kind'] != kind:
            continue
        data = manifest(p) if kind == 'npm' else tomllib.loads((ROOT / p['path'] / 'Cargo.toml').read_text())
        for name, requirement in data.get('dependencies', {}).items():
            local = (isinstance(requirement, str) and requirement.startswith('workspace:')
                     if kind == 'npm' else isinstance(requirement, dict) and 'path' in requirement)
            if not local or name in selected:
                continue
            if name not in catalog:
                raise ValueError(f"{p['name']}: uncatalogued local dependency {name}")
            dependency = catalog[name]
            encoded = urllib.parse.quote(name, safe='')
            url = (f"https://registry.npmjs.org/{encoded}/{dependency['version']}" if kind == 'npm'
                   else f"https://crates.io/api/v1/crates/{encoded}/{dependency['version']}")
            published = fetch(url)
            if not published or (kind == 'crate' and published['version']['yanked']):
                raise ValueError(f"{p['name']}: prepare unpublished dependency {name} {dependency['version']} too")


def preflight(packages, output, kind):
    if kind in ('npm', 'crate'):
        check_dependencies(packages, kind)
    pending = output / ('pending-' + kind)
    pending.mkdir(exist_ok=True)
    if any(pending.iterdir()):
        raise ValueError(f'Pending directory must be empty: {pending}')
    for p in packages:
        if p['kind'] != kind:
            continue
        files = pending_files(p, output / p['key'])
        for file in files:
            target = pending / (p['key'] if kind != 'pypi' else '')
            target.mkdir(exist_ok=True)
            shutil.copy(file, target)
        summary(f"{p['name']} {p['version']}: {'ready to publish' if files else 'already published (verified hashes)'}")
    if file := os.environ.get('GITHUB_OUTPUT'):
        with open(file, 'a') as stream:
            stream.write(f"{kind}_pending={str(any(f.is_file() for f in pending.rglob('*'))).lower()}\n")
    return pending


def summary(line):
    print(line, flush=True)
    if file := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(file, 'a') as stream:
            stream.write('- ' + line + '\n')


def verify_registry(packages, output, kind):
    for p in packages:
        if p['kind'] != kind:
            continue
        for attempt in range(6):
            if not pending_files(p, output / p['key']):
                break
            if attempt == 5:
                raise ValueError(f"{p['name']} {p['version']}: registry did not expose verified artifacts")
            time.sleep(5)


def publish(packages, output, kind):
    # Reconcile each package just before mutation. A failed/uncertain upload is
    # never blindly repeated: rerun the job with the retained tested artifacts.
    selected = [p for p in packages if p['kind'] == kind]
    if kind == 'crate' and selected and any(pending_files(p, output / p['key']) for p in selected):
        selection = [arg for p in selected for arg in ('-p', p['name'])]
        run('cargo', 'package', '--locked', *selection)
        metadata = json.loads(run('cargo', 'metadata', '--no-deps', '--format-version', '1', capture=True))
        for p in selected:
            archive = Path(metadata['target_directory']) / 'package' / f"{p['name']}-{p['version']}.crate"
            if digest(archive) != digest(next((output / p['key']).glob('*.crate'))):
                raise ValueError(f"{p['name']}: Cargo artifact differs from tested archive")
    for p in selected:
        files = pending_files(p, output / p['key'])
        if not files:
            summary(f"{p['name']} {p['version']}: already published (verified hashes)")
            continue
        if kind == 'npm':
            guard_npm_channel(p)
            run('npm', 'publish', str(files[0]), '--ignore-scripts', '--access', 'public',
                '--registry', 'https://registry.npmjs.org',
                '--tag', 'beta' if p['channel'] == 'beta' else 'latest', '--provenance')
        elif kind == 'crate':
            run('cargo', 'publish', '--locked', '-p', p['name'])
        else:
            raise ValueError('PyPI publication uses the trusted publishing action')
        verify_registry([p], output, kind)
        summary(f"{p['name']} {p['version']}: published and verified in {kind}")


def publish_tags(packages, *, check_only=False):
    sha = run('git', 'rev-parse', 'HEAD', capture=True).strip()
    for p in packages:
        refs = run('git', 'ls-remote', 'origin', 'refs/tags/' + p['tag'], 'refs/tags/' + p['tag'] + '^{}', capture=True)
        hashes = dict(line.split()[::-1] for line in refs.splitlines())
        existing = hashes.get('refs/tags/' + p['tag'] + '^{}', hashes.get('refs/tags/' + p['tag']))
        if existing and existing != sha:
            raise ValueError(f"Package tag {p['tag']} already points to another commit")
        if not existing and not check_only:
            run('gh', 'api', '--method', 'POST', 'repos/{owner}/{repo}/git/refs',
                '-f', 'ref=refs/tags/' + p['tag'], '-f', 'sha=' + sha)
        summary(f"{p['name']} {p['version']}: Git tag {'available' if not existing and check_only else 'verified'} ({sha})")


def verify_artifact_plan(output, packages, revision):
    if ((output / 'revision').read_text().strip() != revision
            or read_json(output / 'plan.json') != packages):
        raise ValueError('Artifact plan/revision does not match checked-out release')
    expected = read_json(output / 'sha256.json')
    actual = {str(f.relative_to(output)): digest(f) for p in packages
              for f in (output / p['key']).iterdir()}
    if expected != actual:
        raise ValueError('Artifact inventory/checksums differ from tested packages')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['plan', 'notes', 'build', 'check', 'publish', 'publish-tags', 'check-tags', 'validate', 'verify'])
    parser.add_argument('--tag')
    parser.add_argument('--all', action='store_true', help='Build all current SDKs for validation only')
    parser.add_argument('--output', type=Path, default=Path('sdk-artifacts'))
    parser.add_argument('--kind', choices=['npm', 'pypi', 'crate', 'go'])
    args = parser.parse_args()
    if args.command == 'validate':
        validate()
        return
    if args.all and args.command != 'build':
        parser.error('--all is only allowed for build')
    if not args.all and not args.tag:
        parser.error('--tag is required unless building all packages')
    packages = catalog_packages() if args.all else plan(args.tag)
    if args.command == 'build' and args.kind:
        packages = [p for p in packages if p['kind'] == args.kind]
    output = args.output.resolve()
    if args.command == 'plan':
        print(json.dumps(packages))
    elif args.command == 'notes':
        print(notes(packages))
    elif args.command == 'build':
        build(packages, output)
    else:
        verify_artifact_plan(output, packages, run('git', 'rev-parse', 'HEAD', capture=True).strip())
        if args.command in ('publish-tags', 'check-tags'):
            if args.command == 'publish-tags':
                for kind in ('npm', 'pypi', 'crate'):
                    verify_registry(packages, output, kind)
            publish_tags(packages, check_only=args.command == 'check-tags')
            return
        if not args.kind or args.kind == 'go':
            parser.error('--kind is required for check/publish')
        if args.command == 'check':
            preflight(packages, output, args.kind)
        elif args.command == 'verify':
            verify_registry(packages, output, args.kind)
        else:
            publish(packages, output, args.kind)


if __name__ == '__main__':
    main()

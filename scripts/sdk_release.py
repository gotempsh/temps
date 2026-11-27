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
import sys
import tarfile
import time
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SEMVER = r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(alpha|beta|rc)\.(0|[1-9]\d*))?'
PYVER = r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:(a|b|rc)(0|[1-9]\d*))?'
# `npm stage publish` first shipped in npm 11.16.0.
MIN_STAGE_NPM = (11, 16, 0)


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


# Protocol before the Rust SDK, analytics core before its consumers.
PUBLISH_FIRST = ('analytics-core', 'temps-plugin-protocol')


def publish_order(key):
    return (key not in PUBLISH_FIRST, key)


def package_tag(key, package, version):
    return f"{package['path']}/v{version}" if package['kind'] == 'go' else f'sdk/{key}/v{version}'


# How a catalog package picks its release versions (`versioning`, default 'server'):
# 'server' releases with every beta and stable server tag at the tag's version;
# 'independent' keeps its own line, bumped from its Conventional Commits;
# 'until-server-passes' is independent until the server version is above every
# version the package has published, then follows the server.
VERSIONING = ('server', 'independent', 'until-server-passes')


def catalog_packages(root=ROOT):
    catalog = read_json(root / '.github/releases/packages.json')
    packages = []
    for key, package in catalog.items():
        if package.get('versioning', 'server') not in VERSIONING:
            raise ValueError(f"{key}: unknown versioning {package['versioning']!r}; "
                             f"use one of {', '.join(VERSIONING)}")
        actual = manifest(package, root)
        if actual['name'] != package['name']:
            raise ValueError(f'{key}: catalog name disagrees with manifest')
        version = actual['version']
        packages.append(dict(package, key=key, version=version,
                             channel=channel(version, package['kind']),
                             tag=package_tag(key, package, version)))
    return sorted(packages, key=lambda p: publish_order(p['key']))


# A libraries-only release publishes the SDKs and the CLI without a server
# build. It reserves a server beta number: `libraries/v0.1.0-beta.64` takes
# 0.1.0-beta.64, so the next server release is v0.1.0-beta.65. The `libraries/`
# prefix keeps it out of every server tag pattern (`v*` in release.yml's tag
# filters, validate-release-ref.sh, install.sh, cliff.toml). Only betas can be
# reserved; to change which numbers this mode reserves, change this prefix and
# pattern and `release_tag_problems`.
LIBRARIES_PREFIX = 'libraries/'
LIBRARIES_TAG = LIBRARIES_PREFIX + r'(v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-beta\.(0|[1-9]\d*))'
SERVER_TAG = r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?'
NIGHTLY_TAG = r'v\d+\.\d+\.\d+-nightly\.[\w.-]+'


def is_libraries_tag(tag):
    return bool(re.fullmatch(LIBRARIES_TAG, tag))


def server_tag_of(tag):
    """The server version tag a release tag stands for (`libraries/vX` reserves `vX`)."""
    match = re.fullmatch(LIBRARIES_TAG, tag)
    return match[1] if match else tag


def tag_version(tag):
    """Precedence of a server or libraries release tag, or None (nightly, `v0.1.0-test`, other tags)."""
    tag = server_tag_of(tag)
    if re.fullmatch(NIGHTLY_TAG, tag) or not re.fullmatch(SEMVER, tag.removeprefix('v')):
        return None
    return version_order(tag.removeprefix('v'))


def release_tag_problems(tag, existing, *, ordering=True):
    """Why `tag` cannot be released next given the `existing` repository tags ([] when it can).

    A server tag and a libraries tag of the same version would publish the SDKs
    twice under one number, so each refuses the other. With `ordering` (when the
    tag is about to be created, not when an existing tag is re-checked), a
    libraries tag must be above every server and libraries release, and a server
    tag above every libraries release, so no registry version goes backwards.
    """
    if not (is_libraries_tag(tag) or re.fullmatch(SERVER_TAG, tag)) or re.fullmatch(NIGHTLY_TAG, tag):
        return []
    version = server_tag_of(tag)
    counterpart = version if is_libraries_tag(tag) else LIBRARIES_PREFIX + version
    problems = []
    if counterpart in existing:
        problems.append(f'{version} is already taken by {counterpart}; '
                        f'release the next free number ({next_beta_tag(existing)}) instead')
    if ordering and (order := tag_version(tag)) is not None:
        rivals = [other for other in existing if other != tag and other != counterpart
                  and (is_libraries_tag(other) or is_libraries_tag(tag))
                  and (other_order := tag_version(other)) is not None and other_order >= order]
        if rivals:
            newest = max(rivals, key=tag_version)
            problems.append(f'{tag} would go backwards: {newest} is already released at or above it; '
                            f'release {next_beta_tag(existing)} or later')
    return problems


def next_beta_tag(existing):
    """`vX.Y.Z-beta.N` one above the newest server or libraries release tag.

    Both modes number from the same line, so a libraries-only release's number
    is never reused by the server and the reverse.
    """
    releases = [tag for tag in existing if tag_version(tag) is not None]
    if not releases:
        raise ValueError('No server or libraries release tag exists to number from; pass the version explicitly')
    newest = server_tag_of(max(releases, key=tag_version))
    match = re.fullmatch(SEMVER, newest.removeprefix('v'))
    if match[4] != 'beta':
        raise ValueError(f'The newest release {newest} is not a beta, so there is no next beta number; '
                         f'pass the version explicitly')
    return f'v{match[1]}.{match[2]}.{match[3]}-beta.{int(match[5]) + 1}'


def release_tags(root=ROOT):
    """Every server and libraries release tag in the local repository (fetch tags first)."""
    return run('git', 'tag', '--list', 'v*', LIBRARIES_PREFIX + 'v*', cwd=root, capture=True).split()


def validate(root=ROOT):
    catalog_packages(root)
    for tag in read_json(root / '.github/releases/prepared.json'):
        plan(tag, root)


def plan(tag, root=ROOT):
    catalog = read_json(root / '.github/releases/packages.json')
    prepared = read_json(root / '.github/releases/prepared.json')
    if re.fullmatch(NIGHTLY_TAG, tag):
        if tag in prepared:
            raise ValueError('Nightly server releases cannot prepare SDK releases')
        return []
    elif is_libraries_tag(tag):
        selected = prepared.get(tag, {})
        expected_channel = 'beta'
    elif tag.startswith(LIBRARIES_PREFIX):
        raise ValueError(f'{tag}: a libraries-only release tag is libraries/vX.Y.Z-beta.N')
    elif re.fullmatch(SERVER_TAG, tag):
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
    return sorted(result, key=lambda p: publish_order(p['key']))


def notes(packages):
    lines = ['\n## Prepared SDK package versions\n',
             'Publication is tracked by the Publish prepared SDK releases job; this table records intent, not registry success.\n',
             '| Package | Version | Channel | Package tag |', '| --- | --- | --- | --- |']
    lines += [f"| `{p['name']}` | `{p['version']}` | {p['channel']} | `{p['tag']}` |" for p in packages]
    return '\n'.join(lines) + '\n' if packages else '\nNo SDK releases are prepared for this release tag.\n'


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
            # Packing runs with --ignore-scripts, so honour a prepublishOnly that
            # regenerates sources first: @temps-sdk/api is selected by changes to
            # apps/temps-cli/openapi.json and must ship the client for that spec.
            if runs_generate(data['scripts'].get('prepublishOnly', '')):
                run('bun', 'run', 'generate', cwd=path)
            # Workspace consumers require core declarations before their build.
            if '@temps-sdk/analytics-core' in data.get('dependencies', {}):
                run('bun', 'run', 'build', cwd=ROOT / 'sdks/node/packages/analytics-core')
            if 'test:run' in data['scripts']:
                run('bun', 'run', 'test:run', cwd=path)
            elif data['scripts'].get('test') == 'bun test':
                run('bun', 'test', cwd=path)
            elif data['scripts'].get('test') == 'vitest':
                run('bun', 'x', '--no-install', 'vitest', 'run', cwd=path)
            if p['key'] == 'cli':
                run('bun', 'test', 'src/commands/docs.test.ts', cwd=path)
            run('bun', 'run', 'build', cwd=path)
            if p['key'] in ('node-sdk', 'api', 'workflows', 'kv', 'blob', 'sandbox', 'plugin'):
                run('node', str(ROOT / 'scripts/sdk-package.test.mjs'), str(path))
            artifact = dest / 'package.tgz'
            run('bun', 'pm', 'pack', '--ignore-scripts', '--filename', str(artifact), cwd=path)
            check_npm_archive(artifact, p)
            run('npm', 'pack', str(artifact), '--dry-run', '--ignore-scripts', cwd=dest)
            if p['key'] == 'cli':
                # Exercise the distributable under Node, not Bun/source.
                with tempfile.TemporaryDirectory(prefix='temps-cli-package-') as temporary:
                    run('npm', 'install', '--ignore-scripts', '--no-audit', '--no-fund',
                        str(artifact), cwd=temporary)
                    entry = Path(temporary) / 'node_modules/@temps-sdk/cli/dist/index.js'
                    version = run('node', str(entry), '--version', cwd=temporary, capture=True)
                    if version.strip() != p['version']:
                        raise ValueError('Packaged CLI reports the wrong version')
                    run('node', str(entry), '--help', cwd=temporary)

        elif p['kind'] == 'pypi':
            # A stale committed lockfile makes every local `uv run` in the
            # package rewrite it, so fail here instead (e.g. after adding a
            # [build-system], which turns the root from virtual to editable).
            if (path / 'uv.lock').exists():
                run('uv', 'lock', '--check', cwd=path)
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


def runs_generate(script):
    """Whether a package script runs `bun run generate` itself (`generate:types` is another script)."""
    return bool(re.search(r'(?:^|[\s;&|(])bun run generate(?=$|[\s;&|)])', script))


def registry_json(url, parse=json.load, missing=(404,)):
    """Fetch registry metadata; only the listed statuses mean "does not exist"."""
    request = urllib.request.Request(url, headers={'User-Agent': 'temps-sdk-release (https://github.com/gotempsh/temps)'})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return parse(response)
        except urllib.error.HTTPError as error:
            error.close()
            if error.code in missing:
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
                raise ValueError(f"{p['name']}: uncataloged local dependency {name}")
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


def npm_version_tuple(raw):
    match = re.fullmatch(r'(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?', raw.strip())
    return tuple(map(int, match.groups())) if match else None


def require_stage_npm():
    """Fail before any upload when this npm cannot stage packages."""
    raw = run('npm', '--version', capture=True).strip()
    found = npm_version_tuple(raw)
    if found is None or found < MIN_STAGE_NPM:
        wanted = '.'.join(map(str, MIN_STAGE_NPM))
        raise ValueError(f'npm {raw} cannot stage packages (`npm stage` needs npm {wanted} or later); nothing was uploaded')


def approval_steps(waiting, refused=()):
    """Every waiting version in publish order, so a dependency is never left out of the order.

    `refused` marks versions this run could not stage: they keep their place,
    and nothing after one may be approved until it is public.
    """
    names = ', '.join(f"{p['name']}@{p['version']}" + (' (npm refused to stage it in this run)' if p in refused else '')
                      for p in waiting)
    blocked = ('Before approving anything listed after a refused version, make that version public: approve it '
               'if an earlier run staged it, otherwise fix the refusal and rerun this job. ') if refused else ''
    return (f'Approve the staged npm versions, in this order so dependencies are public first: {names}. '
            f'{blocked}Approve them on npmjs.com, or run `npm stage list` and `npm stage approve <stage-id>` '
            '(both need your login and 2FA). Then rerun this job: it skips what is already public '
            'and creates the package tags.')


def stage_npm(selected, output):
    """Stage npm versions for a maintainer to approve. Never publishes directly.

    The trusted publisher allows only `npm stage publish`, so a stolen CI token
    cannot make a version installable: a maintainer approves each one with 2FA.
    """
    waiting = []
    for p in selected:
        files = pending_files(p, output / p['key'])
        if files:
            waiting.append((p, files[0]))
        else:
            summary(f"{p['name']} {p['version']}: already published (verified hashes)")
    if not waiting:
        return
    require_stage_npm()
    # Every channel guard before the first upload, so a refusal stages nothing.
    for p, _ in waiting:
        guard_npm_channel(p)
    refused = []
    for p, archive in waiting:
        try:
            run('npm', 'stage', 'publish', str(archive), '--ignore-scripts', '--access', 'public',
                '--registry', 'https://registry.npmjs.org',
                '--tag', 'beta' if p['channel'] == 'beta' else 'latest', '--provenance')
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            # Keep going: one refusal (such as a version staged by an earlier
            # run and not approved yet) must not leave the rest unstaged.
            refused.append(p)
            summary(f"{p['name']} {p['version']}: npm refused to stage it (see the log above)")
            continue
        summary(f"{p['name']} {p['version']}: staged on npm, waiting for approval")
    summary(approval_steps([p for p, _ in waiting], refused))
    if refused:
        names = ', '.join(f"{p['name']}@{p['version']}" for p in refused)
        raise ValueError(f'npm refused to stage {names}. If npm says the version already exists, an earlier '
                         'run staged it: approve it (or reject it), then rerun this job.')


def require_npm_approved(packages, output, fetch=registry_json):
    """Before package tags: every selected npm version must be public with the tested bytes."""
    waiting = [p for p in packages if p['kind'] == 'npm' and pending_files(p, output / p['key'], fetch)]
    if waiting:
        raise ValueError('Not public on npm yet, so no package tag was created. ' + approval_steps(waiting))


def publish(packages, output, kind):
    # Reconcile each package just before mutation. A failed/uncertain upload is
    # never blindly repeated: rerun the job with the retained tested artifacts.
    selected = [p for p in packages if p['kind'] == kind]
    if kind == 'npm':
        stage_npm(selected, output)
        return
    if kind == 'crate' and selected and any(pending_files(p, output / p['key']) for p in selected):
        selection = [arg for p in selected for arg in ('-p', p['name'])]
        # --no-verify here and on `cargo publish`: verifying compiles the locked
        # dependency graph, running third-party build scripts and proc macros
        # while this job holds the crates.io token and OIDC credentials. The
        # unprivileged build job already compiled, tested and packaged these
        # crates; the digest check below and the registry checksum check after
        # upload tie what is published to that tested archive.
        run('cargo', 'package', '--locked', '--no-verify', *selection)
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
        if kind == 'crate':
            run('cargo', 'publish', '--locked', '--no-verify', '-p', p['name'])
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


def prepare_command(args, parser):
    """Exit 0 when prepared, 1 when files would change (--check), 2 when the check itself failed."""
    if not args.tag:
        parser.error('--tag is required for prepare')
    if args.at_tag and (not args.check or args.candidate):
        parser.error('--at-tag re-derives a tagged selection; use it with --check and without --candidate')
    # Everything but the verdict runs inside this block, outputs included: a failure
    # anywhere must never read as "files would change" (exit 1).
    try:
        pending = run_prepare(args)
    except Exception as error:  # noqa: BLE001 -- every failure maps to exit 2, with its message
        parser.exit(2, f'error: {error}\n' if isinstance(error, ValueError) else f'error: {type(error).__name__}: {error}\n')
    if pending:
        print(pending, file=sys.stderr)
        sys.exit(1)


def run_prepare(args):
    """Run prepare and write its outputs; returns the pending-edits message, or None."""
    import sdk_prepare
    if args.at_tag:
        files, released = sdk_prepare.verify_tag(args.tag)
        if released:
            print(f"{args.tag}: {', '.join(released)} already released; verifying against the pre-release state")
        if files:
            return (f'error: the SDK selection prepared for {args.tag} is stale at the tag: commits since the '
                    f'prep PR need different versions. These files would change: ' + ', '.join(files))
        print(f'{args.tag}: the prepared SDK selection matches the tagged commit')
        return None
    if args.candidate:
        if found := sdk_prepare.branch_only_changes(args.candidate):
            changes = '; '.join(f"{key}: {', '.join(files)}" for key, files in found)
            raise ValueError(f'{args.candidate} changes SDK files that main does not have ({changes}); '
                             f'land them on main first so they are versioned')
    report, files = sdk_prepare.prepare(args.tag, write=not args.check)
    text = sdk_prepare.summary(args.tag, report)
    print(text, flush=True)
    if args.summary:
        args.summary.write_text(text)
    if args.changed_files:
        args.changed_files.write_text(''.join(path + '\n' for path in files))
    if file := os.environ.get('GITHUB_OUTPUT'):
        with open(file, 'a') as stream:
            stream.write(f'selected={str(bool(report)).lower()}\n')
            stream.write(f"kinds={json.dumps(sorted({entry['kind'] for entry in report}))}\n")
            stream.write(f'changed={str(bool(files)).lower()}\n')
    if args.check and files:
        return f'SDK releases for {args.tag} are not prepared; these files would change: ' + ', '.join(files)
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['plan', 'notes', 'build', 'check', 'publish', 'publish-tags', 'check-tags',
                                            'validate', 'verify', 'prepare', 'check-release-tag', 'next-beta'],
                        help='check-release-tag: refuse a server or libraries tag whose number another release '
                             'took or passed; next-beta: print the next free vX.Y.Z-beta.N for either mode')
    parser.add_argument('--tag')
    parser.add_argument('--all', action='store_true', help='Build all current SDKs for validation only')
    parser.add_argument('--output', type=Path, default=Path('sdk-artifacts'))
    parser.add_argument('--kind', choices=['npm', 'pypi', 'crate', 'go'])
    parser.add_argument('--check', action='store_true',
                        help='prepare: change nothing; exit 1 when files would change, 2 when the check fails')
    parser.add_argument('--summary', type=Path, help='prepare: write the per-package changelog table here')
    parser.add_argument('--changed-files', type=Path, help='prepare: write the changed paths here, one per line')
    parser.add_argument('--candidate', help='prepare: fail when this release branch has SDK commits that main lacks')
    parser.add_argument('--at-tag', action='store_true',
                        help='prepare --check: re-derive the selection on the checked-out, already tagged commit; '
                             'check-release-tag: the tag exists, so only refuse a number taken twice')
    args = parser.parse_args()
    if args.command == 'next-beta':
        try:
            print(next_beta_tag(release_tags()))
        except ValueError as error:
            parser.exit(1, f'error: {error}\n')
        return
    if args.command == 'check-release-tag':
        if not args.tag:
            parser.error('--tag is required for check-release-tag')
        if problems := release_tag_problems(args.tag, release_tags(), ordering=not args.at_tag):
            parser.exit(1, ''.join(f'error: {problem}\n' for problem in problems))
        print(f'{args.tag}: the release number is free')
        return
    if args.command == 'validate':
        validate()
        return
    if args.command == 'prepare':
        prepare_command(args, parser)
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
                require_npm_approved(packages, output)
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

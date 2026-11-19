# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Prepare SDK release versions for a server tag from Conventional Commits.

For every catalog package this derives the next version from the commits that
touched it since its last package tag, bumps the manifests, the lockfiles and
`.github/releases/prepared.json`, and leaves publication to `release.yml`.
It reads git and the public registries only: it never publishes, pushes or
creates tags. The same repository state and registry contents always produce
the same files, so running it twice changes nothing.
"""
from dataclasses import dataclass, field
import json
from pathlib import Path
import re
import subprocess
import tomllib
import urllib.parse

import sdk_release as release

LEVELS = (None, 'patch', 'minor', 'major')
# Pre-1.0 packages treat a breaking change as minor and a feature as a patch.
PRE_1_0 = {'major': 'minor', 'minor': 'patch', 'patch': 'patch'}
SUBJECT = re.compile(r'(?P<type>[a-z]+)(?:\([^()\r\n]*\))?(?P<breaking>!)?: \S')
BREAKING_FOOTER = re.compile(r'^BREAKING[ -]CHANGE: ', re.M)
BUMPS = {'feat': 'minor', 'fix': 'patch', 'perf': 'patch'}
SERVER_TAG = r'v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?'
NIGHTLY_TAG = r'v\d+\.\d+\.\d+-nightly\.[\w.-]+'
MANIFEST = {'npm': 'package.json', 'pypi': 'pyproject.toml', 'crate': 'Cargo.toml', 'go': 'VERSION'}
PRE_RANK = {'alpha': 0, 'beta': 1, 'rc': 2, None: 3}
PEP440_LABEL = {'alpha': 'a', 'beta': 'b', 'rc': 'rc'}
MAX_LISTED_COMMITS = 50


def commit_level(subject, body=''):
    """Bump implied by one Conventional Commit, or None when it releases nothing."""
    match = SUBJECT.match(subject)
    if not match:
        return None
    if match['breaking'] or BREAKING_FOOTER.search(body or ''):
        return 'major'
    return BUMPS.get(match['type'])


def highest(levels):
    return max(levels, key=LEVELS.index, default=None)


@dataclass(frozen=True)
class Version:
    major: int
    minor: int
    patch: int
    pre: str | None = None
    number: int = 0

    @classmethod
    def parse(cls, text, kind):
        match = re.fullmatch(release.PYVER if kind == 'pypi' else release.SEMVER, text)
        if not match:
            raise ValueError(f'Unsupported {kind} version: {text}')
        label = {'a': 'alpha', 'b': 'beta'}.get(match[4], match[4])
        return cls(int(match[1]), int(match[2]), int(match[3]), label, int(match[5] or 0))

    @property
    def core(self):
        return Version(self.major, self.minor, self.patch)

    def key(self):
        return (self.major, self.minor, self.patch, PRE_RANK[self.pre], self.number)

    def channel(self):
        return 'stable' if self.pre is None else 'beta'

    def format(self, kind):
        core = f'{self.major}.{self.minor}.{self.patch}'
        if self.pre is None:
            return core
        if kind == 'pypi':
            return f'{core}{PEP440_LABEL[self.pre]}{self.number}'
        return f'{core}-{self.pre}.{self.number}'


def next_core(base, level):
    """Release core after `base` for `level`, the way node-semver's inc() treats prereleases.

    A prerelease core already contains the bump that created it, so 1.3.0-beta.2
    plus a feature stays 1.3.0 while 1.2.3-beta.2 plus a feature becomes 1.3.0.
    """
    if level is None:
        return base.core
    if base.major == 0:
        level = PRE_1_0[level]
    major, minor, patch = base.major, base.minor, base.patch
    if base.pre is None:
        return {'major': Version(major + 1, 0, 0), 'minor': Version(major, minor + 1, 0),
                'patch': Version(major, minor, patch + 1)}[level]
    if level == 'major':
        return Version(major, 0, 0) if minor == 0 and patch == 0 else Version(major + 1, 0, 0)
    if level == 'minor':
        return Version(major, minor, 0) if patch == 0 else Version(major, minor + 1, 0)
    return base.core


class GitHistory:
    """Read-only view of the repository history prepare needs."""

    def __init__(self, root=release.ROOT):
        self.root = Path(root)

    def git(self, *args):
        return subprocess.run(('git', *args), cwd=self.root, check=True, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=300).stdout

    def ensure_full_history(self):
        if self.git('rev-parse', '--is-shallow-repository').strip() == 'true':
            raise ValueError('SDK preparation needs the full history and tags (checkout with fetch-depth: 0)')

    def tags(self, prefix):
        return self.git('for-each-ref', '--format=%(refname:strip=2)', f'refs/tags/{prefix}*').split()

    def tag_exists(self, tag):
        return bool(self.git('tag', '--list', tag).strip())

    def commits(self, base, paths, excludes):
        """Non-merge commits after `base` that touched `paths`, newest first."""
        spec = [*paths, *(f':(exclude){path}' for path in excludes)]
        output = self.git('log', '--no-merges', '--format=%H%x1f%s%x1f%b%x1e', f'{base}..HEAD', '--', *spec)
        records = [record.strip('\n') for record in output.split('\x1e') if record.strip()]
        return [dict(zip(('sha', 'subject', 'body'), record.split('\x1f', 2))) for record in records]

    def revisions(self, path):
        return self.git('log', '--format=%H', '--', path).split()

    def show(self, revision, path):
        try:
            return self.git('show', f'{revision}:{path}')
        except subprocess.CalledProcessError:
            return None


def go_escape(module):
    # The module proxy protocol encodes capitals as '!' + lowercase.
    return re.sub(r'[A-Z]', lambda match: '!' + match[0].lower(), module)


def published_versions(package, fetch=release.registry_json):
    """Every version string the package's registry has ever accepted (yanked included)."""
    name, kind = package['name'], package['kind']
    encoded = urllib.parse.quote(name, safe='')
    if kind == 'npm':
        data = fetch(f'https://registry.npmjs.org/{encoded}')
        if not data:
            return set()
        # npm never reuses an unpublished version number; `time` still lists it.
        return set(data.get('versions', {})) | (set(data.get('time', {})) - {'created', 'modified'})
    if kind == 'pypi':
        data = fetch(f'https://pypi.org/pypi/{encoded}/json')
        return set(data['releases']) if data else set()
    if kind == 'crate':
        versions, query = set(), '?per_page=100'
        while query:
            data = fetch(f'https://crates.io/api/v1/crates/{encoded}/versions{query}')
            if not data:
                break
            versions |= {version['num'] for version in data['versions']}
            query = data.get('meta', {}).get('next_page')
        return versions
    listing = fetch(f'https://proxy.golang.org/{go_escape(name)}/@v/list',
                    parse=lambda response: response.read().decode(), missing=(404, 410))
    return {line.strip().removeprefix('v') for line in (listing or '').splitlines() if line.strip()}


def parse_all(versions, kind):
    parsed = set()
    for text in versions:
        try:
            parsed.add(Version.parse(text, kind))
        except ValueError:
            continue
    return parsed


def manifest_version(kind, text):
    if kind == 'npm':
        return json.loads(text).get('version')
    if kind == 'go':
        return text.strip()
    data = tomllib.loads(text)
    return data.get('project' if kind == 'pypi' else 'package', {}).get('version')


def last_published_revision(history, package, registry):
    """(version, commit) of the newest published manifest version and the commit that set it.

    Packages released before package tags existed have no tag to diff from; the
    commit that set the manifest to the version the registry has is the
    implicit last release.
    """
    kind = package['kind']
    path = f"{package['path']}/{MANIFEST[kind]}"
    found = None
    for revision in history.revisions(path):
        text = history.show(revision, path)
        try:
            version = Version.parse(manifest_version(kind, text), kind) if text else None
        except (ValueError, TypeError):
            version = None
        if found is None:
            if version in registry:
                found = (version, revision)
        elif version == found[0]:
            found = (version, revision)
        else:
            break
    return found


def package_paths(package, catalog):
    paths = [package['path'], *package.get('sources', [])]
    nested = sorted(other['path'] for other in catalog
                    if other['key'] != package['key']
                    and any(other['path'].startswith(path + '/') for path in paths))
    return paths, nested


def canonical(name):
    return re.sub(r'[-_.]+', '-', name).lower()


def local_dependencies(package, catalog, root):
    """Catalog siblings this package ships a dependency on through the workspace."""
    path = root / package['path']
    kind = package['kind']
    siblings = {(p['name'] if kind != 'pypi' else canonical(p['name'])): p['key']
                for p in catalog if p['kind'] == kind and p['key'] != package['key']}
    found = set()
    if kind == 'npm':
        data = release.read_json(path / 'package.json')
        for section in ('dependencies', 'peerDependencies', 'optionalDependencies'):
            for name, requirement in data.get(section, {}).items():
                if isinstance(requirement, str) and requirement.startswith('workspace:') and name in siblings:
                    found.add(siblings[name])
    elif kind == 'crate':
        data = tomllib.loads((path / 'Cargo.toml').read_text())
        tables = [data.get('dependencies', {}), data.get('build-dependencies', {})]
        tables += [target.get(section, {}) for target in data.get('target', {}).values()
                   for section in ('dependencies', 'build-dependencies')]
        for table in tables:
            for name, spec in table.items():
                if isinstance(spec, dict) and 'path' in spec and spec.get('package', name) in siblings:
                    found.add(siblings[spec.get('package', name)])
    elif kind == 'pypi':
        data = tomllib.loads((path / 'pyproject.toml').read_text())
        sources = {canonical(name): spec for name, spec in
                   data.get('tool', {}).get('uv', {}).get('sources', {}).items()}
        for requirement in data.get('project', {}).get('dependencies', []):
            name = canonical(re.match(r'[A-Za-z0-9][A-Za-z0-9._-]*', requirement)[0])
            spec = sources.get(name)
            if name in siblings and isinstance(spec, dict) and ('path' in spec or spec.get('workspace')):
                found.add(siblings[name])
    else:
        text = (path / 'go.mod').read_text()
        for module, key in siblings.items():
            local = re.search(rf'^\s*(?:replace\s+)?{re.escape(module)}(?:\s+v\S+)?\s+=>\s+\.', text, re.M)
            if local and re.search(rf'^\s*(?:require\s+)?{re.escape(module)}\s+v\S+[ \t]*$', text, re.M):
                found.add(key)
    return found


@dataclass
class Assessment:
    package: dict
    current: Version
    known: set
    published: set
    previous: Version | None = None
    unreleased: bool = False
    commits: list = field(default_factory=list)
    level: str | None = None
    dependencies: list = field(default_factory=list)
    version: Version | None = None

    def selected(self, target):
        graduation = target == 'stable' and self.previous is not None and self.previous.pre is not None
        return self.unreleased or self.level is not None or graduation or bool(self.dependencies)

    def reason(self, target):
        reasons = []
        if self.unreleased:
            reasons.append('first release (never published)')
        if self.commits:
            count = len(self.commits)
            reasons.append(f"{count} commit{'s' if count != 1 else ''} ({self.level})")
        if self.dependencies:
            reasons.append('dependency ' + ', '.join(self.dependencies))
        if not reasons and target == 'stable':
            reasons.append('stable release of the published prerelease')
        return '; '.join(reasons)


def assess(package, catalog, history, published):
    kind = package['kind']
    current = Version.parse(package['version'], kind)
    prefix = release.package_tag(package['key'], package, '')
    tagged = {}
    for tag in history.tags(prefix):
        try:
            tagged[tag] = Version.parse(tag.removeprefix(prefix), kind)
        except ValueError:
            continue
    registry = parse_all(published(package), kind)
    state = Assessment(package, current, known=registry | set(tagged.values()), published=registry)
    if tagged:
        tag, state.previous = max(tagged.items(), key=lambda item: item[1].key())
        base = f'refs/tags/{tag}'
    elif anchor := last_published_revision(history, package, registry):
        state.previous, base = anchor
    elif registry and current in registry:
        raise ValueError(f"{package['key']}: no commit sets {package['name']} to the published {package['version']} "
                         f"in {package['path']}/{MANIFEST[kind]}; it cannot anchor the next release")
    else:
        state.unreleased = True
        return state
    paths, nested = package_paths(package, catalog)
    for commit in history.commits(base, paths, nested):
        level = commit_level(commit['subject'], commit.get('body', ''))
        if level:
            state.commits.append(dict(commit, level=level))
    state.level = highest(commit['level'] for commit in state.commits)
    return state


def next_beta(core, known, floor=0):
    """Next `core`-beta.N above every published or tagged beta of that core (and at least `floor`)."""
    numbers = [version.number for version in known if version.core == core and version.pre == 'beta']
    return Version(core.major, core.minor, core.patch, 'beta', max(floor, max(numbers, default=0) + 1))


def choose_version(state, target):
    package, kind = state.package, state.package['kind']
    if state.unreleased:
        core = state.current.core
        if target == 'stable':
            candidate = core
        else:
            floor = state.current.number if state.current.pre == 'beta' else 0
            candidate = next_beta(core, state.known, floor)
    else:
        level = highest([state.level, 'patch' if state.dependencies else None])
        core = next_core(state.previous, level)
        candidate = core if target == 'stable' else next_beta(core, state.known)
        # A hand-bumped, still unpublished manifest (say a deliberate 1.0.0) wins when it is ahead.
        if (state.current not in state.known and state.current.channel() == target
                and state.current.key() > candidate.key()):
            candidate = state.current
    name = f"{package['name']} {candidate.format(kind)}"
    if candidate in state.known:
        raise ValueError(f'{name} is already published or tagged; reconcile the registry and package tags first')
    newer = sorted((v for v in state.published if v.pre is None and v.key() >= candidate.core.key()), key=Version.key)
    if newer:
        raise ValueError(f"{name}: the registry already has {newer[-1].format(kind)}; "
                         f"the manifest and package tags are behind it")
    if kind == 'go' and candidate.major >= 2:
        raise ValueError(f'{name}: Go v2+ requires a new /v2 module path and catalog entry')
    return candidate


class Edits:
    """Pending file contents, applied together so check mode can compare without writing."""

    def __init__(self, root):
        self.root = root
        self.files = {}

    def text(self, path):
        if path not in self.files:
            self.files[path] = (self.root / path).read_text()
        return self.files[path]

    def replace(self, path, pattern, replacement, *, expected=1, what):
        new, count = re.subn(pattern, replacement, self.text(path), flags=re.M)
        if count != expected:
            raise ValueError(f'{path}: expected {expected} {what}, found {count}')
        self.files[path] = new

    def changed(self):
        return sorted(path for path, text in self.files.items() if (self.root / path).read_text() != text)

    def write(self):
        for path in self.changed():
            (self.root / path).write_text(self.files[path])


def set_manifest_version(edits, package, version):
    kind, old = package['kind'], package['version']
    path = f"{package['path']}/{MANIFEST[kind]}"
    if kind == 'go':
        edits.files[path] = version + '\n'
        return
    if kind == 'npm':
        text = edits.text(path)
        new = re.sub(rf'^(\s*"version"\s*:\s*"){re.escape(old)}(")', rf'\g<1>{version}\g<2>', text, count=1, flags=re.M)
        data, expected = json.loads(new), json.loads(text)
        expected['version'] = version
        if data != expected:
            raise ValueError(f'{path}: could not rewrite the top-level version {old}')
        edits.files[path] = new
        return
    section = 'project' if kind == 'pypi' else 'package'
    text = edits.text(path)
    start = re.search(rf'^\[{section}\]\s*$', text, re.M)
    end = re.search(r'^\[', text[start.end():], re.M) if start else None
    stop = start.end() + end.start() if end else len(text)
    body, count = re.subn(rf'^(version\s*=\s*"){re.escape(old)}(")', rf'\g<1>{version}\g<2>',
                          text[start.end():stop] if start else '', count=1, flags=re.M)
    if not start or count != 1:
        raise ValueError(f'{path}: no literal version = "{old}" in [{section}]')
    edits.files[path] = text[:start.end()] + body + text[stop:]


def set_dependency_pins(edits, package, dependency, version):
    """Point a dependent's in-catalog requirement at the dependency's new version."""
    kind = package['kind']
    path = f"{package['path']}/{MANIFEST[kind] if kind != 'go' else 'go.mod'}"
    name = re.escape(dependency['name'])
    if kind == 'crate':
        pattern = re.compile(rf'^\s*"?{name}"?\s*=\s*\{{.*\bversion\s*=\s*"[=^~]?([^"]+)"')
        lines = edits.text(path).split('\n')
        for index, line in enumerate(lines):
            match = pattern.match(line)
            if match and re.search(r'\bpath\s*=', line):
                lines[index] = line[:match.start(1)] + version + line[match.end(1):]
        edits.files[path] = '\n'.join(lines)
    elif kind == 'pypi':
        # PEP 503: `temps_core`, `temps-core` and `Temps.Core` name the same project.
        spelled = '[-_.]+'.join(map(re.escape, re.split(r'[-_.]+', dependency['name'])))
        edits.files[path] = re.sub(rf'(["\']{spelled}\s*==\s*)[^"\';,\s]+', rf'\g<1>{version}', edits.text(path),
                                   flags=re.I)
    elif kind == 'go':
        edits.files[path] = re.sub(rf'^(\s*(?:require\s+)?{name}\s+)v\S+(?=[ \t]*$)', rf'\g<1>v{version}',
                                   edits.text(path), flags=re.M)


def set_lockfile_versions(edits, package, version, root):
    kind, old, name = package['kind'], package['version'], package['name']
    if kind == 'npm':
        directory = (root / package['path']).parent
        while directory != root and not (directory / 'bun.lock').exists():
            directory = directory.parent
        lock = directory / 'bun.lock'
        if directory == root or not lock.exists():
            return  # A standalone package's own lockfile does not record its version.
        workspace = (root / package['path']).relative_to(directory).as_posix()
        edits.replace(lock.relative_to(root).as_posix(),
                      rf'("{re.escape(workspace)}": \{{\s*"name": "{re.escape(name)}",\s*"version": "){re.escape(old)}(")',
                      rf'\g<1>{version}\g<2>', what=f'workspace entry for {name} {old}')
    elif kind == 'pypi':
        lock = f"{package['path']}/uv.lock"
        if (root / lock).exists():
            edits.replace(lock, rf'^(name = "{re.escape(canonical(name))}"\nversion = "){re.escape(old)}("\nsource = \{{ (?:editable|virtual) = )',
                          rf'\g<1>{version}\g<2>', what=f'editable entry for {name} {old}')
    elif kind == 'crate':
        edits.replace('Cargo.lock', rf'^(name = "{re.escape(name)}"\nversion = "){re.escape(old)}("\n(?!source = ))',
                      rf'\g<1>{version}\g<2>', what=f'path package entry for {name} {old}')


def prepare(tag, root=release.ROOT, *, history=None, published=None, write=True):
    """Select and version SDK releases for server tag `tag`; returns (selection, changed files)."""
    if re.fullmatch(NIGHTLY_TAG, tag):
        raise ValueError('Nightly server releases never prepare SDK releases')
    if not re.fullmatch(SERVER_TAG, tag):
        raise ValueError(f'Not a server release tag: {tag}')
    root = Path(root)
    if history is None:
        history = GitHistory(root)
        history.ensure_full_history()
    if published is None:
        published = published_versions
    if history.tag_exists(tag):
        raise ValueError(f'{tag} is already tagged; prepare SDK releases for the next server tag')
    target = 'beta' if '-' in tag else 'stable'
    catalog = release.catalog_packages(root)
    by_key = {package['key']: package for package in catalog}
    states = {package['key']: assess(package, catalog, history, published) for package in catalog}
    depends = {key: local_dependencies(by_key[key], catalog, root) for key in by_key}
    # Re-release dependents so registry consumers get the new dependency, to a fixed point.
    changed = True
    while changed:
        changed = False
        for key in sorted(states):
            state = states[key]
            cascade = sorted(dep for dep in depends[key] if states[dep].selected(target))
            if cascade and not state.selected(target):
                state.dependencies = [by_key[dep]['name'] for dep in cascade]
                changed = True
    selection = [key for key in sorted(states, key=release.publish_order) if states[key].selected(target)]
    for key in selection:
        states[key].version = choose_version(states[key], target)
    final = {key: (states[key].version.format(by_key[key]['kind']) if key in selection else by_key[key]['version'])
             for key in by_key}
    # Same rule as plan(): a stable release never depends on a prerelease sibling.
    for key in selection if target == 'stable' else []:
        for dep in sorted(depends[key]):
            if release.channel(final[dep], by_key[dep]['kind']) != 'stable':
                raise ValueError(f"{key}: stable package depends on prerelease sibling {by_key[dep]['name']} {final[dep]}")
    edits = Edits(root)
    for key in selection:
        package, version = by_key[key], final[key]
        if version != package['version']:
            set_manifest_version(edits, package, version)
            set_lockfile_versions(edits, package, version, root)
        for dependent in sorted(k for k in by_key if key in depends[k]):
            set_dependency_pins(edits, by_key[dependent], package, version)
    prepared = {tag: {key: final[key] for key in selection}} if selection else {}
    edits.files['.github/releases/prepared.json'] = json.dumps(prepared, indent=2) + '\n'
    files = edits.changed()
    report = [dict(key=key, name=by_key[key]['name'], kind=by_key[key]['kind'], version=final[key],
                   previous=states[key].previous.format(by_key[key]['kind']) if states[key].previous else None,
                   channel=target, tag=release.package_tag(key, by_key[key], final[key]),
                   reason=states[key].reason(target),
                   commits=[{k: c[k] for k in ('sha', 'subject', 'level')} for c in states[key].commits])
              for key in selection]
    if write:
        edits.write()
        release.validate(root)
        planned = [(p['key'], p['version']) for p in release.plan(tag, root)]
        if planned != [(entry['key'], entry['version']) for entry in report]:
            raise ValueError(f'{tag}: prepared selection does not round-trip through plan()')
    return report, files


def inline(text):
    # Inline code keeps commit subjects from mentioning users or rendering markdown.
    return '`' + text.replace('`', "'") + '`'


def summary(tag, report):
    lines = [f'## SDK releases prepared for {inline(tag)}', '']
    if not report:
        return '\n'.join(lines + ['No SDK changed since its last release; nothing to prepare.', ''])
    lines += [
        f'Merging this records the selection in `.github/releases/prepared.json`. The `release.yml` run for '
        f'{inline(tag)} then tests, publishes and tags exactly these versions; this PR publishes nothing.',
        '',
        'Before 1.0.0 a breaking change bumps the minor version and a feature bumps the patch.',
        '',
        '| Package | Registry | Previous | Next | Why |',
        '| --- | --- | --- | --- | --- |',
    ]
    for entry in report:
        previous = inline(entry['previous']) if entry['previous'] else 'none'
        lines.append(f"| {inline(entry['name'])} | {entry['kind']} | {previous} | {inline(entry['version'])} | {entry['reason']} |")
    for entry in report:
        if not entry['commits']:
            continue
        count = len(entry['commits'])
        lines += ['', f"<details><summary>{entry['name']} {entry['version']}: {count} commit{'s' if count != 1 else ''}</summary>", '']
        for commit in entry['commits'][:MAX_LISTED_COMMITS]:
            lines.append(f"- {commit['sha'][:9]} {inline(commit['subject'])} ({commit['level']})")
        hidden = len(entry['commits']) - MAX_LISTED_COMMITS
        if hidden > 0:
            lines.append(f'- and {hidden} more')
        lines += ['', '</details>']
    return '\n'.join(lines) + '\n'

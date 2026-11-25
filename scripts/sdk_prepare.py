# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Prepare SDK release versions for a server tag.

SDKs follow the server: every catalog package releases with every beta and stable
server tag, at the tag's version (`v0.1.0-beta.64` releases `0.1.0-beta.64`, or
`0.1.0b64` on PyPI). A package whose catalog entry sets `versioning` keeps its own
version line instead, derived from the Conventional Commits that touched it since
its last package tag (see `sdk_release.VERSIONING`). Preparation bumps the manifests, the
lockfiles and `.github/releases/prepared.json`, and leaves publication to `release.yml`.
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
# Types are case-insensitive in Conventional Commits 1.0; the BREAKING CHANGE footer is not.
SUBJECT = re.compile(r'(?P<type>[A-Za-z]+)(?:\([^()\r\n]*\))?(?P<breaking>!)?: \S')
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
    return BUMPS.get(match['type'].lower())


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


def server_version(tag):
    """The version a server tag gives the SDKs that follow it."""
    try:
        return Version.parse(tag.removeprefix('v'), 'npm')
    except ValueError:
        raise ValueError(f'{tag}: SDKs release at the server version, which must be X.Y.Z or '
                         f'X.Y.Z-(alpha|beta|rc).N') from None


def follows_server(state, server):
    """Whether a package releases at the server version `server` (catalog `versioning`)."""
    mode = state.package.get('versioning', 'server')
    if mode == 'until-server-passes':
        # Already ahead of the server (the CLI shipped 0.1.38 before SDKs followed it):
        # keep its own line until the server version is above everything it published.
        return not at_or_above(state, server, inclusive=True)
    return mode == 'server'


def at_or_above(state, version, *, inclusive):
    """Package-tag and registry versions of the package above `version` (or equal, when inclusive).

    Both are ordered by the registry's own rules, so a version `Version` cannot
    represent (an npm `0.3.0-canary.1`, a PyPI `0.2.0.dev1`) still counts, and one
    differing only in build metadata (`0.1.0-beta.64+build.5`) is equal. A version
    that cannot be ordered at all stops preparation.
    """
    kind, name = state.package['kind'], state.package['name']
    key = precedence(version.format(kind), kind)
    found = []
    for source, texts in (('package tag', state.tag_texts), ('registry version', state.registry_texts)):
        for text in sorted(texts):
            other = precedence(text, kind)
            if other is None:
                raise ValueError(f'{name}: cannot order {source} {text!r} against {version.format(kind)}; '
                                 f'resolve it before preparing')
            if other > key or (inclusive and other == key):
                found.append(text)
    return found


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

    def reachable_tags(self):
        """Every tag on HEAD's history (HEAD included)."""
        return set(self.git('for-each-ref', '--merged', 'HEAD', '--format=%(refname:strip=2)', 'refs/tags/').split())

    def resolve(self, ref):
        """The commit `ref` names, or '' when it names none."""
        try:
            return self.git('rev-parse', '--verify', '--quiet', f'{ref}^{{commit}}').strip()
        except subprocess.CalledProcessError:
            return ''

    def commits(self, base, paths, excludes):
        """Non-merge commits after `base` that touched `paths`, newest first."""
        spec = [*paths, *(f':(exclude){path}' for path in excludes)]
        output = self.git('log', '--no-merges', '--format=%H%x1f%s%x1f%b%x1e', f'{base}..HEAD', '--', *spec)
        records = [record.strip('\n') for record in output.split('\x1e') if record.strip()]
        return [dict(zip(('sha', 'subject', 'body'), record.split('\x1f', 2))) for record in records]

    def changed_files(self, candidate, paths, excludes):
        """Paths `candidate` changed since its merge base with HEAD and still differ from HEAD.

        A fix landed on both branches (cherry-picked to main) has identical content and
        is not reported; a change made only on `candidate`, merge resolutions included, is.
        """
        # --no-renames reports both sides of a rename (a deleted source must not hide
        # behind an identical destination); -z keeps names with spaces whole.
        spec = [*paths, *(f':(exclude){path}' for path in excludes)]
        changed = self.names('diff', '--no-renames', '--name-only', '-z', f'HEAD...{candidate}', '--', *spec)
        if not changed:
            return []
        differ = set(self.names('diff', '--no-renames', '--name-only', '-z', 'HEAD', candidate, '--',
                                *changed))
        return [path for path in changed if path in differ]

    def names(self, *args):
        return [name for name in self.git(*args).split('\0') if name]

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


PEP440 = re.compile(r'v?(\d+(?:\.\d+)*)(?:[-_.]?(a|b|rc|alpha|beta|c|pre|preview)[-_.]?(\d*))?'
                    r'(?:[-_.]?(post|rev|r)[-_.]?(\d*)|-(\d+))?(?:[-_.]?dev[-_.]?(\d*))?', re.I)


SEMVER_PRECEDENCE = re.compile(r'v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)'
                               r'(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z.-]+)?')
INF = float('inf')


def precedence(text, kind):
    """A sort key that orders any registry version as its registry does, or None.

    SemVer 2.0 precedence for npm, crates.io and Go (any prerelease identifiers,
    build metadata ignored); PEP 440 for PyPI (`.devN` before its pre-releases,
    `.postN` after its release, `1.0` equal to `1.0.0`).
    """
    text = text.strip()
    if kind != 'pypi':
        match = SEMVER_PRECEDENCE.fullmatch(text)
        if not match:
            return None
        core = tuple(int(part) for part in match.group(1, 2, 3))
        if match[4] is None:
            return (core, (1,))
        identifiers = tuple((0, int(part), '') if part.isdigit() else (1, 0, part) for part in match[4].split('.'))
        return (core, (0, identifiers))
    match = PEP440.fullmatch(text)
    if not match:
        return None
    release = [int(part) for part in match[1].split('.')]
    while len(release) > 1 and release[-1] == 0:
        release.pop()
    label = (match[2] or '').lower()
    has_post = bool(match[4]) or match[6] is not None
    post = int((match[5] if match[4] else match[6]) or 0) if has_post else None
    dev = int(match[7] or 0) if match[7] is not None else None
    if label:
        pre = ({'a': 0, 'alpha': 0, 'b': 1, 'beta': 1}.get(label, 2), int(match[3] or 0))
    else:
        pre = (-INF,) if dev is not None and post is None else (INF,)
    return (tuple(release), pre, (-INF,) if post is None else (post,), (INF,) if dev is None else (dev,))


def registry_version(text, kind):
    """A registry version as the Version it occupies, or None when it cannot collide.

    PyPI compares versions by PEP 440, so `1.2` is `1.2.0`, and `1.2.0.post1` or
    `1.2.0.1` sit at or above 1.2.0: both occupy the 1.2.0 release for the
    already-published and registry-ahead guards. A `.devN` build precedes its
    release and blocks nothing.
    """
    if kind != 'pypi':
        try:
            return Version.parse(text, kind)
        except ValueError:
            return None
    match = PEP440.fullmatch(text.strip())
    if not match or match[7] is not None:
        return None
    release_parts = [int(part) for part in match[1].split('.')]
    major, minor, patch = (release_parts + [0, 0, 0])[:3]
    label = {'a': 'alpha', 'alpha': 'alpha', 'b': 'beta', 'beta': 'beta'}.get((match[2] or '').lower(),
                                                                          'rc' if match[2] else None)
    if label:
        return Version(major, minor, patch, label, int(match[3] or 0))
    # Final releases, post-releases and longer release segments occupy the core.
    return Version(major, minor, patch)


def parse_all(versions, kind):
    return {version for text in versions if (version := registry_version(text, kind)) is not None}


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


def cargo_dependencies(data):
    """(key, spec) for every table-form dependency a crate ships with (not dev-dependencies)."""
    tables = [data.get('dependencies', {}), data.get('build-dependencies', {})]
    tables += [target.get(section, {}) for target in data.get('target', {}).values()
               for section in ('dependencies', 'build-dependencies')]
    return [(key, spec) for table in tables for key, spec in table.items() if isinstance(spec, dict)]


def go_patterns(module):
    escaped = re.escape(module)
    return (re.compile(rf'^(\s*(?:require\s+)?{escaped}\s+)v(\S+)(?=[ \t]*(?://.*)?$)', re.M),
            re.compile(rf'^(\s*(?:replace\s+)?{escaped}(?:\s+v(\S+))?\s+=>\s+\.)', re.M))


def go_local_requirement(text, module):
    """Version of `module` this go.mod requires through a local replace, or None.

    A version-qualified replace only applies to that exact required version.
    """
    require, replace = go_patterns(module)
    required = require.search(text)
    if not required:
        return None
    if any(match[2] in (None, required[2]) for match in replace.finditer(text)):
        return required[2]
    return None


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
        for key, spec in cargo_dependencies(tomllib.loads((path / 'Cargo.toml').read_text())):
            if 'path' in spec and spec.get('package', key) in siblings:
                found.add(siblings[spec.get('package', key)])
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
            if go_local_requirement(text, module):
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
    server: Version | None = None  # set when the package releases at the server version
    tag_texts: set = field(default_factory=set)  # this package's release tags, as versions spelled in the tag
    registry_texts: set = field(default_factory=set)  # every registry version, as the registry spells it

    def selected(self, target):
        graduation = target == 'stable' and self.previous is not None and self.previous.pre is not None
        return (self.server is not None or self.unreleased or self.level is not None or graduation
                or bool(self.dependencies))

    def reason(self, target):
        if self.server is not None:
            reasons = ['server version']
            if self.commits:
                count = len(self.commits)
                reasons.append(f"{count} commit{'s' if count != 1 else ''} ({self.level})")
            return '; '.join(reasons)
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
    tagged, tag_texts = {}, set()
    for tag in history.tags(prefix):
        tag_texts.add(tag.removeprefix(prefix))
        try:
            tagged[tag] = Version.parse(tag.removeprefix(prefix), kind)
        except ValueError:
            continue
    texts = set(published(package))
    registry = parse_all(texts, kind)
    state = Assessment(package, current, known=registry | set(tagged.values()), published=registry,
                       tag_texts=tag_texts, registry_texts=texts)
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
    if state.server is not None:
        candidate = state.server
        name = f"{package['name']} {candidate.format(kind)}"
        # Equal precedence counts as taken: registries refuse a version that differs only in build metadata.
        if candidate in state.known or at_or_above(state, candidate, inclusive=True) != at_or_above(
                state, candidate, inclusive=False):
            raise ValueError(f'{name} is already published or tagged; reconcile the registry and package tags first')
        if ahead := at_or_above(state, candidate, inclusive=False):
            raise ValueError(f"{name}: {', '.join(ahead)} {'is' if len(ahead) == 1 else 'are'} already published "
                             f"or tagged, above the "
                             f"server version; set \"versioning\": \"until-server-passes\" in its catalog entry")
        if kind == 'go' and candidate.major >= 2:
            raise ValueError(f'{name}: Go v2+ requires a new /v2 module path and catalog entry')
        return candidate
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
        """Write every change, or restore the originals when any write fails."""
        # Bytes, so a rollback also restores the original line endings.
        originals = {path: (self.root / path).read_bytes() for path in self.changed()}
        self.originals = originals
        try:
            for path in originals:
                (self.root / path).write_text(self.files[path])
        except BaseException:
            self.restore()
            raise

    def restore(self):
        for path, data in getattr(self, 'originals', {}).items():
            (self.root / path).write_bytes(data)


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
    new = text[:start.end()] + body + text[stop:] if start else text
    expected = tomllib.loads(text)
    expected.setdefault(section, {})['version'] = version
    if not start or count != 1 or tomllib.loads(new) != expected:
        raise ValueError(f'{path}: no literal version = "{old}" in [{section}] to rewrite')
    edits.files[path] = new


def set_dependency_pins(edits, package, dependency, version):
    """Point a dependent's exact in-catalog requirement at the dependency's new version.

    Floating requirements (`workspace:*`, a Cargo path dependency without a
    version) follow the dependency by themselves. Every edit is re-parsed and
    fails closed when a pin still names another version.
    """
    kind, name = package['kind'], dependency['name']
    path = f"{package['path']}/{MANIFEST[kind] if kind != 'go' else 'go.mod'}"
    text = edits.text(path)
    if kind == 'npm':
        new = re.sub(rf'("{re.escape(name)}"\s*:\s*"workspace:[\^~]?)\d[^"]*(")', rf'\g<1>{version}\g<2>', text)
        data = json.loads(new)
        stale = [requirement for section in ('dependencies', 'peerDependencies', 'optionalDependencies')
                 for dep, requirement in data.get(section, {}).items()
                 if dep == name and re.fullmatch(r'workspace:[\^~]?\d.*', requirement)
                 and re.sub(r'^workspace:[\^~]?', '', requirement) != version]
    elif kind == 'crate':
        lines = text.split('\n')
        for index, line in enumerate(lines):
            table = re.match(r'\s*("?)([\w-]+)\1\s*=\s*\{(.*)\}', line)
            if not table or not re.search(r'\bpath\s*=', table[3]):
                continue
            renamed = re.search(r'\bpackage\s*=\s*"([^"]+)"', table[3])
            if (renamed[1] if renamed else table[2]) != name:
                continue
            lines[index] = re.sub(r'(\bversion\s*=\s*"\s*[=^~]?\s*)[^"]+(")', rf'\g<1>{version}\g<2>', line)
        new = '\n'.join(lines)
        stale = [spec['version'] for key, spec in cargo_dependencies(tomllib.loads(new))
                 if spec.get('package', key) == name and 'path' in spec and 'version' in spec
                 and spec['version'].lstrip('=^~ ') != version]
    elif kind == 'pypi':
        # PEP 503: `temps_core`, `temps-core` and `Temps.Core` name the same project.
        # Extras (`temps-core[fast]==1.0.0`) stay as they are.
        spelled = '[-_.]+'.join(map(re.escape, re.split(r'[-_.]+', name))) + r'\s*(?:\[[^\]]*\])?'
        new = re.sub(rf'(["\']{spelled}\s*==\s*)[^"\';,\s]+', rf'\g<1>{version}', text, flags=re.I)
        stale = [requirement for requirement in tomllib.loads(new).get('project', {}).get('dependencies', [])
                 if re.match(rf'{spelled}\s*==', requirement, re.I) and not re.match(
                     rf'{spelled}\s*==\s*{re.escape(version)}\s*(?:[;,]|$)', requirement, re.I)]
    else:
        require, replace = go_patterns(name)
        required = require.search(text)
        new = require.sub(rf'\g<1>v{version}', text)
        if required:
            # A replace pinned to the old required version must move with it.
            new = replace.sub(lambda match: match[0].replace(f'v{required[2]}', f'v{version}')
                              if match[2] == required[2] else match[0], new)
        stale = [] if go_local_requirement(new, name) in (None, version) else [name]
    if stale:
        raise ValueError(f'{path}: could not repin {name} to {version} (still {", ".join(stale)}); edit it by hand')
    edits.files[path] = new


def bun_workspace(package, root):
    """(workspace root, bun.lock path) of the bun workspace an npm package belongs to, if any."""
    directory = (root / package['path']).parent
    while directory != root and not (directory / 'bun.lock').exists():
        directory = directory.parent
    if directory == root or not (directory / 'bun.lock').exists():
        return None
    return directory, (directory / 'bun.lock').relative_to(root).as_posix()


def set_lockfile_ranges(edits, package, version, root, dependents):
    """Keep the explicit ranges bun.lock records for npm dependents equal to their manifests.

    `set_dependency_pins` rewrites `workspace:^0.0.6` in a dependent's package.json; its
    bun.lock entry records the same range, which a fresh `bun install` would rewrite too.
    This runs even when `package` keeps its manifest version (a manual bump).
    """
    if package['kind'] != 'npm' or not (found := bun_workspace(package, root)):
        return
    directory, path = found
    name = package['name']
    for dependent in dependents:
        try:
            key = (root / dependent['path']).relative_to(directory).as_posix()
        except ValueError:
            continue  # Another lockfile's workspace.
        entry = re.compile(rf'^(    "{re.escape(key)}": \{{\n)(.*?)(^    \}},)', re.M | re.S)
        edits.files[path] = entry.sub(lambda match: match[1] + re.sub(
            rf'("{re.escape(name)}": "workspace:[\^~]?)\d[^"]*(")', rf'\g<1>{version}\g<2>', match[2]) + match[3],
            edits.text(path))


def set_lockfile_versions(edits, package, version, root, dependents=()):
    kind, old, name = package['kind'], package['version'], package['name']
    if kind == 'npm':
        if not (found := bun_workspace(package, root)):
            return  # A standalone package's own lockfile does not record its version.
        directory, path = found
        workspace = (root / package['path']).relative_to(directory).as_posix()
        edits.replace(path,
                      rf'("{re.escape(workspace)}": \{{\s*"name": "{re.escape(name)}",\s*"version": "){re.escape(old)}(")',
                      rf'\g<1>{version}\g<2>', what=f'workspace entry for {name} {old}')
    elif kind == 'pypi':
        for lock in [f"{p['path']}/uv.lock" for p in [package, *dependents]]:
            if (root / lock).exists():
                edits.replace(lock, rf'^(name = "{re.escape(canonical(name))}"\nversion = "){re.escape(old)}'
                              rf'("\nsource = \{{ (?:editable|virtual|directory) = )',
                              rf'\g<1>{version}\g<2>', what=f'local entry for {name} {old}')
    elif kind == 'crate':
        edits.replace('Cargo.lock', rf'^(name = "{re.escape(name)}"\nversion = "){re.escape(old)}("\n(?!source = ))',
                      rf'\g<1>{version}\g<2>', what=f'path package entry for {name} {old}')


def set_stamped_versions(edits, package, version):
    """Rewrite `name@old` in committed files generated from the package (catalog `stamped`).

    The CLI's command reference records the CLI version it was generated from, and a
    test keeps it equal to the manifest, so a bump that skipped it would fail the build.
    """
    name, old = package['name'], package['version']
    for path in package.get('stamped', []):
        edits.replace(path, rf'{re.escape(name)}@{re.escape(old)}(?![0-9A-Za-z.+-])', f'{name}@{version}',
                      what=f'{name}@{old} stamp')


# The npm dist-tag that publishes each release channel (see sdk_release.channel).
DIST_TAGS = {'stable': 'latest', 'beta': 'beta'}


def set_channel_pins(edits, package, version):
    """Move `name@<dist-tag>` invocations in committed docs (catalog `channel_pinned` globs).

    The skills run the CLI as `bunx @temps-sdk/cli@latest` while it is stable and
    `@beta` while its version is a prerelease, so the commands they document exist
    in the CLI they run. When a bump changes the channel, every pin moves with it;
    finding none fails closed, because the docs have drifted from the catalog.
    """
    patterns = package.get('channel_pinned', [])
    if not patterns:
        return
    old, new = (DIST_TAGS[release.channel(v, package['kind'])] for v in (package['version'], version))
    if old == new:
        return
    name = package['name']
    pin = re.compile(rf'{re.escape(name)}@{old}(?![\w-])')
    paths = sorted({match.relative_to(edits.root).as_posix()
                    for pattern in patterns for match in edits.root.glob(pattern) if match.is_file()})
    moved = 0
    for path in paths:
        text, count = pin.subn(f'{name}@{new}', edits.text(path))
        if count:
            edits.files[path] = text
            moved += count
    if not moved:
        raise ValueError(f"{package['key']}: no file matching {', '.join(patterns)} pins {name}@{old}; "
                         f'pin the documented commands to @{old} before moving them to @{new}')


def prepare(tag, root=release.ROOT, *, history=None, published=None, write=True, at_tag=False):
    """Select and version SDK releases for server tag `tag`; returns (selection, changed files).

    `at_tag` re-derives the selection on the commit `tag` already points at (read-only).
    """
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
    if at_tag:
        if write:
            raise ValueError('Re-deriving the selection at a server tag never writes files')
        if not history.tag_exists(tag) or history.resolve(tag) != history.resolve('HEAD'):
            raise ValueError(f'{tag} is not the checked-out commit; check out the tag to verify its SDK selection')
    elif history.tag_exists(tag):
        raise ValueError(f'{tag} is already tagged; prepare SDK releases for the next server tag')
    target = 'beta' if '-' in tag else 'stable'
    server = server_version(tag)
    catalog = release.catalog_packages(root)
    by_key = {package['key']: package for package in catalog}
    states = {package['key']: assess(package, catalog, history, published) for package in catalog}
    for key, state in states.items():
        if follows_server(state, server):
            state.server = server
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
    # A selected package ships against its siblings' final versions: an unselected
    # sibling must already be on the registry (sdk_release.check_dependencies).
    for key in selection:
        for dep in sorted(depends[key]):
            if dep not in selection and states[dep].current not in states[dep].published:
                raise ValueError(f"{key}: depends on {by_key[dep]['name']} {final[dep]}, which is neither "
                                 f"published nor releasable now; restore its published version or release it")
    edits = Edits(root)
    for key in selection:
        package, version = by_key[key], final[key]
        dependents = [by_key[k] for k in sorted(by_key) if key in depends[k]]
        if version != package['version']:
            set_manifest_version(edits, package, version)
            set_lockfile_versions(edits, package, version, root, dependents)
            set_stamped_versions(edits, package, version)
            set_channel_pins(edits, package, version)
        for dependent in dependents:
            set_dependency_pins(edits, dependent, package, version)
        set_lockfile_ranges(edits, package, version, root, dependents)
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
        try:
            release.validate(root)
            planned = [(p['key'], p['version']) for p in release.plan(tag, root)]
            if planned != [(entry['key'], entry['version']) for entry in report]:
                raise ValueError(f'{tag}: prepared selection does not round-trip through plan()')
        except BaseException:
            edits.restore()
            raise
    return report, files


def verify_tag(tag, root=release.ROOT, *, history=None, published=None):
    """Re-derive `tag`'s SDK selection on its tagged commit before anything publishes.

    Returns (files that would change, keys already released). A commit that landed
    after the prep PR merged can need different versions than prepared.json records,
    and published versions are immutable. The derivation sees the registries and tags
    as they were before this release: its own package tags and registry versions are
    hidden (a re-run after part of it was published still verifies every package), and
    so are later releases, whose package tags are not on the tagged commit's history
    (a fresh run at an older tag still matches its selection).
    """
    root = Path(root)
    if history is None:
        history = GitHistory(root)
        history.ensure_full_history()
    if published is None:
        published = published_versions
    planned = release.plan(tag, root)
    if not planned:
        return [], []
    released = [entry['key'] for entry in planned if history.tag_exists(entry['tag'])
                or Version.parse(entry['version'], entry['kind']) in parse_all(published(entry), entry['kind'])]
    hidden_tags = {entry['tag'] for entry in planned}
    # By catalog key: packages on different registries can share a name (the Python and
    # Rust plugin SDKs are both `temps-plugin-sdk`).
    # Each hidden version by `Version` and by registry precedence, so a later release
    # `Version` cannot represent (an npm canary) is hidden too.
    hidden = {}

    def hide(key, text, kind):
        versions, keys = hidden.setdefault(key, (set(), set()))
        if (order := precedence(text, kind)) is not None:
            keys.add(order)
        try:
            versions.add(Version.parse(text, kind))
        except ValueError:
            pass

    for entry in planned:
        hide(entry['key'], entry['version'], entry['kind'])
    reachable = history.reachable_tags()
    for package in release.catalog_packages(root):
        prefix = release.package_tag(package['key'], package, '')
        for later in (tag for tag in history.tags(prefix) if tag not in reachable):
            hidden_tags.add(later)
            hide(package['key'], later.removeprefix(prefix), package['kind'])

    def before_release(package):
        versions, keys = hidden.get(package['key'], (set(), set()))
        kind = package['kind']
        return {text for text in published(package)
                if registry_version(text, kind) not in versions and precedence(text, kind) not in keys}

    _, files = prepare(tag, root, history=BeforeRelease(history, hidden_tags), published=before_release,
                       write=False, at_tag=True)
    return files, released


class BeforeRelease:
    """A history without the package tags of this release and of later ones."""

    def __init__(self, history, hidden):
        self.history, self.hidden = history, hidden

    def __getattr__(self, name):
        return getattr(self.history, name)

    def tags(self, prefix):
        return [tag for tag in self.history.tags(prefix) if tag not in self.hidden]

    def tag_exists(self, tag):
        return tag not in self.hidden and self.history.tag_exists(tag)


RELEASE_METADATA = '.github/releases'


def branch_only_changes(candidate, root=release.ROOT, *, history=None):
    """SDK files `candidate` (a release branch) changes relative to where it left HEAD (main).

    Preparation reads main, so an SDK change made only on the release branch would merge
    into the tag without a version; it must land on main first. This compares content
    (`git diff HEAD...candidate`), so a change made in a merge commit's resolution counts
    as much as one in an ordinary commit. Packages, and catalog entries, added only on
    the branch count too. Returns [(key, [paths])]; a server version bump returns [].
    """
    root = Path(root)
    history = history or GitHistory(root)
    if not history.resolve(candidate):
        raise ValueError(f'{candidate} does not name a commit; fetch the release branch first')
    catalog = release.catalog_packages(root)
    known = {package['key'] for package in catalog}
    branch = json.loads(history.show(candidate, f'{RELEASE_METADATA}/packages.json') or '{}')
    catalog += [dict(package, key=key) for key, package in branch.items() if key not in known]
    found = []
    for package in catalog:
        paths, nested = package_paths(package, catalog)
        if files := history.changed_files(candidate, paths, nested):
            found.append((package['key'], files))
    if files := history.changed_files(candidate, [RELEASE_METADATA], []):
        found.append(('release metadata', files))
    return found


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
        'SDKs release at the server version. A package whose catalog entry sets `versioning` keeps its own '
        'version line, bumped from its commits: before 1.0.0 a breaking change bumps the minor version and '
        'a feature bumps the patch.',
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

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  StartError,
  consoleApiUrl,
  databaseUrl,
  generateAdminPassword,
  isLocalServerCommand,
  isLocalState,
  localPaths,
  localUrls,
  parseChannel,
  parseChecksum,
  parseJsonOutput,
  parseLocalServerPids,
  parsePort,
  pickRelease,
  proxyPortCandidates,
  releaseAssetUrl,
  releaseTarget,
  resolveSkillsScope,
  volumeOwnership,
  type WorkloadContainer,
  skillTargets,
} from './runtime.js'

describe('releaseTarget', () => {
  test('maps macOS and Linux on both architectures', () => {
    expect(releaseTarget('darwin', 'arm64')).toBe('temps-darwin-arm64')
    expect(releaseTarget('darwin', 'x64')).toBe('temps-darwin-amd64')
    expect(releaseTarget('linux', 'x64')).toBe('temps-linux-amd64')
    expect(releaseTarget('linux', 'arm64')).toBe('temps-linux-arm64')
  })

  test('rejects Windows with a pointer to the server installer', () => {
    expect(() => releaseTarget('win32', 'x64')).toThrow(/win32\/x64.*deploy\.sh/)
  })
})

describe('pickRelease', () => {
  const asset = (name: string) => [{ name: `${name}.tar.gz` }]
  const releases = [
    { tag_name: 'v0.1.0-nightly.20261010.abc', prerelease: true, assets: asset('temps-darwin-arm64') },
    { tag_name: 'v0.1.0-beta.57', prerelease: true, assets: asset('temps-linux-amd64') },
    { tag_name: 'v0.1.0-beta.56', prerelease: true, assets: asset('temps-darwin-arm64') },
    { tag_name: 'v0.0.9', draft: true, assets: asset('temps-darwin-arm64') },
    { tag_name: 'v0.0.8', prerelease: false, assets: asset('temps-darwin-arm64') },
  ]

  test('beta is the newest non-nightly with a build for this machine', () => {
    expect(pickRelease(releases, 'beta', 'temps-darwin-arm64')).toBe('v0.1.0-beta.56')
    expect(pickRelease(releases, 'beta', 'temps-linux-amd64')).toBe('v0.1.0-beta.57')
  })

  test('nightly only picks nightly builds', () => {
    expect(pickRelease(releases, 'nightly', 'temps-darwin-arm64')).toBe('v0.1.0-nightly.20261010.abc')
    expect(pickRelease(releases, 'nightly', 'temps-linux-amd64')).toBeNull()
  })

  test('stable skips prereleases and drafts', () => {
    expect(pickRelease(releases, 'stable', 'temps-darwin-arm64')).toBe('v0.0.8')
  })
})

describe('parseChecksum', () => {
  const digest = 'a'.repeat(64)

  test('reads the digest from a sha256sum line', () => {
    expect(parseChecksum(`${digest}  temps-darwin-arm64.tar.gz\n`)).toBe(digest)
    expect(parseChecksum(digest.toUpperCase())).toBe(digest)
  })

  test('rejects anything that is not a SHA-256 digest', () => {
    expect(() => parseChecksum('<html>Not Found</html>')).toThrow(StartError)
  })
})

describe('parseJsonOutput', () => {
  test('skips a log line printed before the JSON', () => {
    const stdout = '2026-10-11T05:01:17Z INFO Using Docker context colima\n{\n  "id": 1,\n  "api_key": "tk_x"\n}\n'
    expect(parseJsonOutput<{ api_key: string }>(stdout, 'api-key').api_key).toBe('tk_x')
  })

  test('names the command when there is no JSON', () => {
    expect(() => parseJsonOutput('done', 'setup')).toThrow(/temps setup/)
  })
})

describe('generateAdminPassword', () => {
  test('always has upper, lower, digit and symbol', () => {
    for (let i = 0; i < 50; i++) {
      const password = generateAdminPassword()
      expect(password).toMatch(/[A-Z]/)
      expect(password).toMatch(/[a-z]/)
      expect(password).toMatch(/[0-9]/)
      expect(password).toMatch(/[^A-Za-z0-9]/)
      expect(password.length).toBeGreaterThanOrEqual(24)
    }
  })

  test('uses the random source it is given', () => {
    const zeros = (n: number) => Buffer.alloc(n)
    expect(generateAdminPassword(zeros)).toBe(`Tl-${'A'.repeat(20)}-9a`)
  })
})

describe('URLs', () => {
  test('omits :80 and keeps other ports', () => {
    expect(localUrls(80).console).toBe('http://console.127-0-0-1.sslip.io')
    expect(localUrls(8080).app('my-app')).toBe('http://my-app.127-0-0-1.sslip.io:8080')
  })

  test('the CLI talks to the console listener directly', () => {
    expect(consoleApiUrl(8700)).toBe('http://127.0.0.1:8700/api')
  })

  test('the database password is URL-encoded', () => {
    expect(databaseUrl({ dbPort: 55432, dbPassword: 'a/b@c' })).toBe(
      'postgresql://temps:a%2Fb%40c@127.0.0.1:55432/temps?sslmode=disable',
    )
  })

  test('release assets come from the GitHub release', () => {
    expect(releaseAssetUrl('v0.1.0-beta.56', 'temps-darwin-arm64', '.tar.gz.sha256')).toBe(
      'https://github.com/gotempsh/temps/releases/download/v0.1.0-beta.56/temps-darwin-arm64.tar.gz.sha256',
    )
  })
})

describe('proxyPortCandidates', () => {
  test('tries the requested port first, then 8080 upwards', () => {
    expect(proxyPortCandidates(3001, 'darwin', false).slice(0, 3)).toEqual([3001, 8080, 8081])
  })

  test('only tries port 80 for root on Linux', () => {
    expect(proxyPortCandidates(undefined, 'linux', true)[0]).toBe(80)
    expect(proxyPortCandidates(undefined, 'linux', false)).not.toContain(80)
    expect(proxyPortCandidates(undefined, 'darwin', true)).not.toContain(80)
  })
})

describe('resolveSkillsScope', () => {
  test('installs into a project when the folder looks like one', () => {
    expect(resolveSkillsScope(undefined, ['src', 'package.json'])).toBe('project')
    expect(resolveSkillsScope(undefined, ['.git'])).toBe('project')
  })

  test('installs for the user anywhere else', () => {
    expect(resolveSkillsScope(undefined, ['Downloads', 'Desktop'])).toBe('global')
  })

  test('honours an explicit scope and rejects unknown ones', () => {
    expect(resolveSkillsScope('none', ['package.json'])).toBe('none')
    expect(() => resolveSkillsScope('everywhere', [])).toThrow(/project, global or none/)
  })

  test('targets the Claude Code and shared agent skill folders', () => {
    expect(skillTargets('/repo')).toEqual(['/repo/.claude/skills', '/repo/.agents/skills'])
  })
})

describe('parseChannel and paths', () => {
  test('defaults to beta and rejects unknown channels', () => {
    expect(parseChannel(undefined)).toBe('beta')
    expect(() => parseChannel('edge')).toThrow(/stable, beta, nightly/)
  })

  test('keeps everything under ~/.temps/local', () => {
    const paths = localPaths('/home/me')
    expect(paths.state).toBe('/home/me/.temps/local/state.json')
    expect(paths.log).toBe('/home/me/.temps/local/server.log')
  })
})

describe('isLocalServerCommand', () => {
  const paths = localPaths('/home/me')
  const serve = 'serve --address 127.0.0.1:8080 --console-address 127.0.0.1:8700'

  test('accepts the server temps start launched', () => {
    expect(isLocalServerCommand(`/home/me/.temps/local/bin/v0.1.0-beta.56/temps ${serve}\n`, paths)).toBe(true)
    expect(isLocalServerCommand('/home/me/.temps/local/bin/v1.2.3/temps serve', paths)).toBe(true)
  })

  test('accepts a home folder with spaces', () => {
    const spaced = localPaths('/Users/Ada Lovelace')
    expect(isLocalServerCommand(`/Users/Ada Lovelace/.temps/local/bin/v1.2.3/temps ${serve}`, spaced)).toBe(true)
  })

  test('rejects a reused pid: another program, another temps, another subcommand', () => {
    expect(isLocalServerCommand('/usr/bin/vim notes.txt', paths)).toBe(false)
    expect(isLocalServerCommand(`/usr/local/bin/temps ${serve}`, paths)).toBe(false)
    expect(isLocalServerCommand('/home/me/.temps/local/bin/v1.2.3/temps setup --auto', paths)).toBe(false)
    expect(isLocalServerCommand('/home/me/.temps/local/bin/v1.2.3/other/temps serve', paths)).toBe(false)
    expect(isLocalServerCommand('', paths)).toBe(false)
  })
})

describe('parseLocalServerPids', () => {
  test('finds the local servers in a process listing and nothing else', () => {
    const paths = localPaths('/home/me')
    const listing = [
      '    1 /sbin/init',
      '  812 /home/me/.temps/local/bin/v1.2.3/temps serve --address 127.0.0.1:8080',
      ' 9001 /usr/local/bin/temps serve --address 0.0.0.0:80',
      '12345 /home/me/.temps/local/bin/v1.2.4/temps serve',
      ' 4242 /home/me/.temps/local/bin/v1.2.3/temps api-key --name x',
      ' 5555 vim /home/me/.temps/local/bin/v1.2.3/temps serve',
      '',
    ].join('\n')
    expect(parseLocalServerPids(listing, paths)).toEqual([812, 12345])
    expect(parseLocalServerPids('', paths)).toEqual([])
  })
})

describe('parsePort', () => {
  test('accepts whole port numbers', () => {
    expect(parsePort('1')).toBe(1)
    expect(parsePort('8080')).toBe(8080)
    expect(parsePort('65535')).toBe(65535)
  })

  test('rejects typos that parseInt would read as a port', () => {
    for (const value of ['8080abc', '8080.5', '80 80', ' 8080', '0x50', '8e3', '+80', '-80', '', '0', '65536', '99999', '123456']) {
      expect(() => parsePort(value)).toThrow(/--port must be a whole number/)
    }
  })
})

describe('isLocalState', () => {
  const valid = {
    version: 'v1.2.3',
    dbPort: 55432,
    dbPassword: 'p',
    proxyPort: 8080,
    consolePort: 8700,
    adminEmail: 'admin@localhost',
    adminPassword: 'p',
    setupComplete: false,
  }

  test('accepts a state file with or without a pid', () => {
    expect(isLocalState(valid)).toBe(true)
    expect(isLocalState({ ...valid, pid: 123 })).toBe(true)
  })

  test('rejects valid JSON that is not a state file', () => {
    for (const value of [null, [], 'x', {}, { ...valid, dbPort: '55432' }, { ...valid, pid: 'abc' }, { ...valid, pid: -1 }, { ...valid, dbPassword: '' }, { ...valid, proxyPort: 70000 }]) {
      expect(isLocalState(value)).toBe(false)
    }
  })
})

describe('volumeOwnership', () => {
  const INSTALLED = 1_000_000
  const volume = (labels: Record<string, string> = {}, createdAt = INSTALLED + 5_000) => ({ name: 'v', createdAt, labels })
  const container = (name: string, labels: Record<string, string>): WorkloadContainer => ({ name, labels, volumes: ['v'] })
  const service = container('redis-cache', { 'temps.service_type': 'redis', 'temps.service_name': 'cache' })
  const app = container('web-1', { 'sh.temps.managed': 'true', 'sh.temps.project_id': '1', 'sh.temps.deploy_id': '7' })
  const stack = container('shop-api-1', { 'sh.temps.managed': 'true', 'com.docker.compose.project': 'shop' })

  test('owns the volume of a managed service, whichever label generation it carries', () => {
    expect(volumeOwnership(volume(), [service], INSTALLED)).toEqual({ owned: true })
    expect(volumeOwnership(volume(), [container('mongo', { 'temps.service': 'mongodb' })], INSTALLED)).toEqual({ owned: true })
    expect(volumeOwnership(volume(), [container('pg', { 'sh.temps.service.type': 'postgres' })], INSTALLED)).toEqual({ owned: true })
    // Created in the same second as the install's own volume.
    expect(volumeOwnership(volume({}, INSTALLED), [service], INSTALLED)).toEqual({ owned: true })
  })

  test('owns a volume Compose created for the stack that mounts it', () => {
    expect(volumeOwnership(volume({ 'com.docker.compose.project': 'shop' }), [stack], INSTALLED)).toEqual({ owned: true })
  })

  test('being newer than the install is not proof: an app with an unlabelled volume keeps it', () => {
    const verdict = volumeOwnership(volume(), [app], INSTALLED)
    expect(verdict).toEqual({ owned: false, reason: 'Temps did not create it (it was attached to web-1)' })
    // An `external` Compose volume has no Compose label, or another stack's.
    expect(volumeOwnership(volume(), [stack], INSTALLED).owned).toBe(false)
    expect(volumeOwnership(volume({ 'com.docker.compose.project': 'other' }), [stack], INSTALLED).owned).toBe(false)
  })

  test('one non-owning user is enough to keep a volume', () => {
    expect(volumeOwnership(volume(), [service, app], INSTALLED).owned).toBe(false)
    expect(volumeOwnership(volume({ 'com.docker.compose.project': 'shop' }), [stack, app], INSTALLED).owned).toBe(false)
  })

  test('an app cannot pass for a managed service with a label from its image', () => {
    for (const marker of ['sh.temps.project_id', 'sh.temps.deploy_id', 'com.docker.compose.project']) {
      const spoof = container('spoof', { 'temps.service_type': 'redis', [marker]: '1' })
      expect(volumeOwnership(volume(), [spoof], INSTALLED).owned).toBe(false)
    }
  })

  test('keeps anything it cannot date against the install', () => {
    expect(volumeOwnership(volume({}, INSTALLED - 1), [service], INSTALLED)).toEqual({ owned: false, reason: 'it existed before this install' })
    expect(volumeOwnership(volume({}, Number.NaN), [service], INSTALLED).owned).toBe(false)
    expect(volumeOwnership(volume(), [service], null).owned).toBe(false)
    expect(volumeOwnership(volume(), [], INSTALLED).owned).toBe(false)
  })
})

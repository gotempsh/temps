// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import golden from './worker-install.golden.json'
import {
  AGENT_SERVICE_COMMAND,
  asRootTemps,
  INSTALLER_URL,
  installCommand,
  releaseTag,
  ROOT_TEMPS_BIN,
  shellWord,
  USER_TEMPS_BIN,
  userInstallCommand,
  type WorkerInstall,
  workerInstallFrom,
  workerJoinScript,
} from './worker-join-script'

// The server's pairing command (crates/temps-core/src/worker_install.rs) is
// tested against the same file, so the two renderers cannot drift.
describe('the commands shared with the server', () => {
  test('install exactly as the golden file says', () => {
    expect(INSTALLER_URL).toBe(golden.installer_url)
    expect(ROOT_TEMPS_BIN).toBe(golden.root_temps_bin)
    expect(USER_TEMPS_BIN).toBe(golden.user_temps_bin)
    expect(golden.installs.length).toBeGreaterThan(0)
    for (const { install, linux, mac } of golden.installs) {
      expect(installCommand(install as WorkerInstall)).toBe(linux)
      expect(userInstallCommand(install as WorkerInstall)).toBe(mac)
    }
    for (const { args, command } of golden.as_root_temps) {
      expect(asRootTemps(args)).toBe(command)
    }
  })

  test('pin the same release tags', () => {
    for (const [version, tag] of golden.release_tags.pinned) {
      expect(releaseTag(version)).toBe(tag)
    }
    for (const version of golden.release_tags.not_pinned) {
      expect(releaseTag(version)).toBeNull()
    }
  })
})

describe('releaseTag', () => {
  test('keeps the release tags the installer can download', () => {
    expect(releaseTag('v0.0.8')).toBe('v0.0.8')
    expect(releaseTag('v0.2.0-beta.1')).toBe('v0.2.0-beta.1')
    expect(releaseTag('v0.1.0-nightly.20261104.0123abcde')).toBe(
      'v0.1.0-nightly.20261104.0123abcde'
    )
    expect(releaseTag('  v1.2.3\n')).toBe('v1.2.3')
    // Hyphens inside a prerelease, as the installer's own tag check allows.
    expect(releaseTag('v1.2.3-rc-1')).toBe('v1.2.3-rc-1')
  })

  test('does not pin what is not a release', () => {
    expect(releaseTag(undefined)).toBeNull()
    expect(releaseTag(null)).toBeNull()
    expect(releaseTag('')).toBeNull()
    expect(releaseTag('dev')).toBeNull()
    expect(releaseTag('0.1.0')).toBeNull()
    expect(releaseTag('v0.1.0-')).toBeNull()
    expect(releaseTag('v0.1.0 (abc1234)')).toBeNull()
  })

  test('never lets shell syntax through into the pasted command', () => {
    for (const hostile of [
      'v1.0.0; rm -rf /',
      'v1.0.0$(id)',
      'v1.0.0`id`',
      'v1.0.0-a|b',
      "v1.0.0-'x'",
      'v1.0.0-a\nb',
    ]) {
      expect(releaseTag(hostile)).toBeNull()
    }
  })
})

describe('installCommand', () => {
  test('pins the control plane release and installs as root', () => {
    expect(
      installCommand({
        kind: 'release',
        tag: 'v0.1.0-nightly.20261104.0123abcde',
      })
    ).toBe(
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- v0.1.0-nightly.20261104.0123abcde)'
    )
  })

  test('falls back to the installer default without a release', () => {
    expect(installCommand({ kind: 'latest' })).toBe(
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash)'
    )
  })

  test('takes the newest nightly for a build between releases', () => {
    expect(installCommand({ kind: 'nightly', after: 'v0.1.0-beta.61' })).toBe(
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- --channel nightly)'
    )
  })

  test('never pastes a tag that is not a release into the command', () => {
    expect(installCommand({ kind: 'release', tag: 'v1.0.0; rm -rf /' })).toBe(
      installCommand({ kind: 'latest' })
    )
  })
})

describe('workerInstallFrom', () => {
  test("uses the server's choice when it reports one", () => {
    expect(
      workerInstallFrom({
        current_version: 'v0.1.0-beta.61',
        worker_install: { kind: 'nightly', after: 'v0.1.0-beta.61' },
      })
    ).toEqual({ kind: 'nightly', after: 'v0.1.0-beta.61' })
  })

  test('pins an older server to its version, and nothing to the newest stable', () => {
    expect(workerInstallFrom({ current_version: 'v0.1.0-beta.58' })).toEqual({
      kind: 'release',
      tag: 'v0.1.0-beta.58',
    })
    expect(workerInstallFrom({ current_version: 'dev' })).toEqual({
      kind: 'latest',
    })
    expect(workerInstallFrom(undefined)).toEqual({ kind: 'latest' })
  })
})

describe('root commands', () => {
  test('call the root install by absolute path, so PATH does not matter', () => {
    expect(asRootTemps('join https://cp.example.com tok')).toBe(
      'sudo -H /root/.temps/bin/temps join https://cp.example.com tok'
    )
    expect(AGENT_SERVICE_COMMAND).toBe(
      'sudo -H /root/.temps/bin/temps agent service install'
    )
  })
})

describe('shellWord', () => {
  test('leaves plain URLs and tokens as they are', () => {
    expect(shellWord('https://cp.example.com:8443/base')).toBe(
      'https://cp.example.com:8443/base'
    )
    expect(shellWord('tk_abc-DEF.123')).toBe('tk_abc-DEF.123')
  })

  test('single-quotes anything with shell syntax, quotes included', () => {
    expect(shellWord('https://cp.example.com/$(id)')).toBe(
      "'https://cp.example.com/$(id)'"
    )
    expect(shellWord('https://cp.example.com/a;b|c`d`')).toBe(
      "'https://cp.example.com/a;b|c`d`'"
    )
    expect(shellWord("it's")).toBe("'it'\\''s'")
    expect(shellWord('a b')).toBe("'a b'")
  })
})

describe('workerJoinScript', () => {
  const base = {
    url: 'https://cp.example.com',
    token: 'tk_enroll',
    install: {
      kind: 'release',
      tag: 'v0.1.0-nightly.20261104.0123abcde',
    } as WorkerInstall,
    joinFlags: ' --ingress delegated',
  }

  test('installs the control plane version, joins and starts the agent with sudo, each step only after the last worked', () => {
    const script = workerJoinScript(base)
    expect(script.install).toEqual(base.install)
    expect(script.command).toBe(
      [
        '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash -s -- v0.1.0-nightly.20261104.0123abcde) && \\',
        'sudo -H /root/.temps/bin/temps join https://cp.example.com tk_enroll --ingress delegated && \\',
        'sudo -H /root/.temps/bin/temps agent service install',
      ].join('\n')
    )
    // Every step that needs root gets it.
    for (const line of script.command.split('\n')) {
      expect(line).toContain('sudo -H')
    }
  })

  test('adds --force only when the user chose to move the machine', () => {
    expect(workerJoinScript(base).command).not.toContain('--force')
    expect(workerJoinScript({ ...base, force: true }).command).toContain(
      'sudo -H /root/.temps/bin/temps join https://cp.example.com tk_enroll --ingress delegated --force && \\'
    )
  })

  test('quotes a join URL with shell syntax instead of running it as root', () => {
    const script = workerJoinScript({
      ...base,
      url: 'https://cp.example.com/$(touch /tmp/x)',
    })
    expect(script.command).toContain(
      "temps join 'https://cp.example.com/$(touch /tmp/x)' tk_enroll"
    )
  })

  test('says so when the version cannot be pinned', () => {
    const script = workerJoinScript({ ...base, install: { kind: 'latest' } })
    expect(script.install).toEqual({ kind: 'latest' })
    expect(script.command.split('\n')[0]).toBe(
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | sudo -H bash) && \\'
    )
  })

  test('a Mac installs and joins as the user, without sudo, and starts a LaunchAgent', () => {
    const script = workerJoinScript({
      ...base,
      install: { kind: 'release', tag: 'v0.1.0-beta.58' },
      os: 'macos',
    })
    expect(script.command).not.toContain('sudo')
    expect(script.command).not.toContain('/root/')
    expect(script.command.split('\n')).toEqual([
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | bash -s -- v0.1.0-beta.58) && \\',
      '~/.temps/bin/temps join https://cp.example.com tk_enroll --ingress delegated && \\',
      '~/.temps/bin/temps agent service install',
    ])
  })

  test('a Mac on a build between releases also takes the newest nightly', () => {
    const script = workerJoinScript({
      ...base,
      install: { kind: 'nightly', after: 'v0.1.0-beta.61' },
      os: 'macos',
    })
    expect(script.command.split('\n')[0]).toBe(
      '(set -o pipefail; curl -fsSL https://temps.sh/install.sh | bash -s -- --channel nightly) && \\'
    )
  })

  test('a Mac join never passes --private-address: it dials out', () => {
    const script = workerJoinScript({ ...base, os: 'macos' })
    expect(script.command).not.toContain('--private-address')
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AiHarnessCredential } from '@/api/client'
import {
  AI_HARNESS_KEY_NAME,
  AI_HARNESS_POLL_INTERVAL_MS,
  buildAiHarnessCommands,
  buildAiHarnessKeyRequest,
  describeHarnessConnection,
  harnessPollInterval,
  harnessStatusOf,
  shouldShowHarnessPendingBanner,
} from './ai-onboarding'

const STUB_BUNX = 'bunx() { printf "BUNX %s\\n" "$*"; }'

/**
 * Feed the copied connect command to a real shell the way a terminal without
 * bracketed paste does: the script and then the user's typed key arrive on
 * the same stdin, line by line. `bunx` is stubbed to print its arguments.
 */
function runConnect(shell: string, typedKey: string) {
  const { connectCli } = buildAiHarnessCommands('https://temps.example.com')
  const stdin = `${STUB_BUNX}\n${connectCli}\n${typedKey}\n`
  const result = Bun.spawnSync([shell], {
    stdin: new TextEncoder().encode(stdin),
    env: { PATH: process.env.PATH ?? '/usr/bin:/bin', HOME: '/tmp' },
  })
  return {
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  }
}

const shells = ['bash', 'zsh'].map((name) => ({
  name,
  available: Bun.which(name) !== null,
}))

describe('AI harness onboarding commands', () => {
  test('installs only the canonical Temps skill', () => {
    expect(
      buildAiHarnessCommands('https://temps.example.com').installSkill
    ).toBe('bunx skills add gotempsh/temps --skill temps')
  })

  test('connects the pinned CLI without putting the API key in command text', () => {
    const commands = buildAiHarnessCommands('https://temps.example.com/')

    expect(commands.connectCli).toContain(
      'bunx @temps-sdk/cli@0.1.38 login https://temps.example.com'
    )
    expect(commands.connectCli).toContain('--api-key "$TEMPS_API_KEY"')
    expect(commands.connectCli).toContain('--context temps-example-com')
    expect(commands.connectCli).toContain('unset TEMPS_API_KEY')
  })

  test('never uses read -p, which zsh treats as a coprocess read', () => {
    const { connectCli } = buildAiHarnessCommands('https://temps.example.com')
    expect(connectCli).not.toMatch(/read\s+-\w*p/)
  })

  for (const shell of shells) {
    test.skipIf(!shell.available)(
      `${shell.name}: logs in with the typed key, never an empty one`,
      () => {
        const { stdout } = runConnect(shell.name, 'tk_typed_example')
        expect(stdout).toContain(
          'BUNX @temps-sdk/cli@0.1.38 login https://temps.example.com --api-key tk_typed_example --context temps-example-com'
        )
      }
    )

    test.skipIf(!shell.available)(
      `${shell.name}: an empty key does not start a browser login`,
      () => {
        const { stdout, stderr } = runConnect(shell.name, '')
        expect(stdout).not.toContain('BUNX')
        expect(stderr).toContain('No API key entered')
      }
    )
  }

  test('verifies the same explicit target context', () => {
    const commands = buildAiHarnessCommands('http://localhost:3003')

    expect(commands.verifyIdentity).toBe(
      'bunx @temps-sdk/cli@0.1.38 --target-context localhost whoami'
    )
    expect(commands.verifyProjects).toContain(
      '--target-context localhost projects list --json'
    )
  })

  test('creates the dedicated harness key with admin access', () => {
    expect(buildAiHarnessKeyRequest()).toEqual({
      name: AI_HARNESS_KEY_NAME,
      role_type: 'admin',
    })
  })
})

describe('AI harness status', () => {
  test('is unknown until the server answers', () => {
    expect(harnessStatusOf(undefined)).toBeUndefined()
    expect(harnessPollInterval(undefined)).toBe(false)
  })

  test('polls only while waiting', () => {
    expect(harnessPollInterval({ status: 'waiting' })).toBe(
      AI_HARNESS_POLL_INTERVAL_MS
    )
    expect(harnessPollInterval({ status: 'connected' })).toBe(false)
    expect(harnessPollInterval({ status: 'missing' })).toBe(false)
  })

  const pending = {
    status: 'waiting' as const,
    pathname: '/projects',
    completedManually: false,
  }

  test('the banner shows while the dedicated key waits', () => {
    expect(shouldShowHarnessPendingBanner(pending)).toBe(true)
  })

  test('the banner clears once any credential connects the harness', () => {
    expect(
      shouldShowHarnessPendingBanner({ ...pending, status: 'connected' })
    ).toBe(false)
  })

  test('the banner clears when the pending key is removed (missing)', () => {
    expect(
      shouldShowHarnessPendingBanner({ ...pending, status: 'missing' })
    ).toBe(false)
  })

  test('the banner clears when the user marked the harness step done', () => {
    expect(
      shouldShowHarnessPendingBanner({ ...pending, completedManually: true })
    ).toBe(false)
  })

  test('the banner stays off the pages that resolve it', () => {
    for (const pathname of [
      '/setup/ai',
      '/settings/keys',
      '/settings/keys/12',
    ]) {
      expect(shouldShowHarnessPendingBanner({ ...pending, pathname })).toBe(
        false
      )
    }
  })

  test('the banner stays hidden while the status is unknown', () => {
    expect(
      shouldShowHarnessPendingBanner({ ...pending, status: undefined })
    ).toBe(false)
  })

  const credential: AiHarnessCredential = {
    id: 3,
    name: 'cli-device:laptop-1767225600',
    key_prefix: 'tk_ab12c',
    kind: 'cli_login',
    last_used_at: '2026-01-01T00:00:00Z',
  }

  test('names a browser-approved CLI login by its key prefix only', () => {
    const line = describeHarnessConnection(credential)
    expect(line).toContain('browser-approved CLI login')
    expect(line).toContain('tk_ab12c…')
  })

  test('names the dedicated key', () => {
    expect(
      describeHarnessConnection({ ...credential, kind: 'dedicated_key' })
    ).toContain(AI_HARNESS_KEY_NAME)
  })

  test('says nothing when no credential connected', () => {
    expect(describeHarnessConnection(null)).toBeNull()
    expect(describeHarnessConnection(undefined)).toBeNull()
  })
})

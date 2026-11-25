// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AiHarnessCredential } from '@/api/client'
import {
  AI_HARNESS_KEY_NAME,
  AI_HARNESS_POLL_INTERVAL_MS,
  buildAiHarnessCommands,
  buildAiHarnessKeyRequest,
  canRemovePendingKey,
  describeHarnessConnection,
  harnessPollInterval,
  harnessStatusOf,
  ONBOARDING_POLL_INTERVAL_MS,
  onboardingPollInterval,
  reconcileCreatedKey,
  resolveOnboardingStatus,
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
  // The trailing line runs in the user's shell after the block: the key must
  // not be left behind there.
  const stdin = `${STUB_BUNX}\n${connectCli}\n${typedKey}\necho "AFTER[\${TEMPS_API_KEY-unset}]"\n`
  const result = Bun.spawnSync([shell], {
    stdin: new TextEncoder().encode(stdin),
    env: { PATH: process.env.PATH ?? '/usr/bin:/bin', HOME: '/tmp' },
  })
  return {
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  }
}

/**
 * Logs every `stty` call to stdout instead of touching a terminal (the
 * command sends `stty`'s stderr to /dev/null); `-g` "saves" a state.
 */
const STUB_STTY =
  'stty() { if [ "$1" = -g ]; then echo saved-state; else echo "STTY $*"; fi; }'

/**
 * Start the connect command, wait until it sits at the hidden key prompt
 * (echo turned off), then interrupt it the way Ctrl-C does.
 */
async function interruptAtPrompt(shell: string) {
  const { connectCli } = buildAiHarnessCommands('https://temps.example.com')
  // `:` after the block keeps the shell from exec-ing the subshell in place,
  // so the subshell is a child process that can be signalled.
  const script = `${STUB_BUNX}\n${STUB_STTY}\n${connectCli}\n:\n`
  const child = Bun.spawn([shell, '-c', script], {
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
    env: { PATH: process.env.PATH ?? '/usr/bin:/bin', HOME: '/tmp' },
  })
  const decoder = new TextDecoder()
  let stdout = ''
  const reader = child.stdout.getReader()
  const deadline = Date.now() + 4_000
  while (!stdout.includes('STTY -echo') && Date.now() < deadline) {
    const { value, done } = await reader.read()
    if (done) break
    stdout += decoder.decode(value)
  }
  // Ctrl-C reaches the foreground job; the subshell is the outer shell's child.
  Bun.spawnSync(['pkill', '-INT', '-P', String(child.pid)])
  const killer = setTimeout(() => child.kill('SIGKILL'), 4_000)
  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    stdout += decoder.decode(value)
  }
  await child.exited
  clearTimeout(killer)
  return { stdout }
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
    // A subshell: the key and the cleanup traps end with the block.
    expect(commands.connectCli.startsWith('(')).toBe(true)
    expect(commands.connectCli.trimEnd().endsWith(')')).toBe(true)
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
      `${shell.name}: the key is not left in the user's shell`,
      () => {
        const { stdout } = runConnect(shell.name, 'tk_typed_example')
        expect(stdout).toContain('AFTER[unset]')
      }
    )

    test.skipIf(!shell.available)(
      `${shell.name}: Ctrl-C at the key prompt restores the terminal and logs nothing in`,
      async () => {
        const { stdout } = await interruptAtPrompt(shell.name)
        expect(stdout).toContain('STTY -echo')
        expect(stdout).toContain('STTY saved-state')
        expect(stdout).not.toContain('BUNX')
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

  test('names an MCP client key, such as the Claude app extension, by its prefix only', () => {
    const line = describeHarnessConnection({
      ...credential,
      name: 'mcp-claude-app-2026-11-25T10-00-00Z',
      kind: 'mcp_client',
    })
    expect(line).toContain('MCP client key')
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

  describe('on /setup/ai', () => {
    const missing = { status: 'missing' as const, key_id: null }
    const waiting = { status: 'waiting' as const, key_id: 12 }

    test('a failed or pending status read is unknown, not missing', () => {
      expect(
        resolveOnboardingStatus(undefined, { keyJustCreated: false })
      ).toBe('unknown')
    })

    test('a successful missing answer offers creation', () => {
      expect(resolveOnboardingStatus(missing, { keyJustCreated: false })).toBe(
        'missing'
      )
    })

    test('a key created in this visit waits even if the refetch failed', () => {
      expect(resolveOnboardingStatus(undefined, { keyJustCreated: true })).toBe(
        'waiting'
      )
      expect(resolveOnboardingStatus(missing, { keyJustCreated: true })).toBe(
        'waiting'
      )
    })

    test('the server answer wins once it reports the key', () => {
      const connected = { status: 'connected' as const, key_id: 12 }
      expect(resolveOnboardingStatus(connected, { keyJustCreated: true })).toBe(
        'connected'
      )
      const deactivated = { status: 'missing' as const, key_id: 12 }
      expect(
        resolveOnboardingStatus(deactivated, { keyJustCreated: true })
      ).toBe('missing')
    })

    test('keeps polling while the page shows waiting', () => {
      expect(onboardingPollInterval(undefined, { keyJustCreated: true })).toBe(
        ONBOARDING_POLL_INTERVAL_MS
      )
      expect(onboardingPollInterval(waiting, { keyJustCreated: false })).toBe(
        ONBOARDING_POLL_INTERVAL_MS
      )
      expect(
        onboardingPollInterval(
          { status: 'connected', key_id: 12 },
          { keyJustCreated: true }
        )
      ).toBe(false)
      expect(onboardingPollInterval(undefined, { keyJustCreated: false })).toBe(
        false
      )
    })

    test('removal goes ahead only while the confirmed key still waits', () => {
      expect(canRemovePendingKey(waiting, 12)).toBe(true)
      expect(canRemovePendingKey(waiting, null)).toBe(false)
      expect(canRemovePendingKey(waiting, 13)).toBe(false)
      expect(canRemovePendingKey({ status: 'connected', key_id: 12 }, 12)).toBe(
        false
      )
      expect(canRemovePendingKey(undefined, 12)).toBe(false)
    })

    describe('a key created in this visit', () => {
      const created = { secret: 'tk_secret', id: 12, acknowledged: false }

      test('stays unacknowledged until the server reports it', () => {
        expect(reconcileCreatedKey(created, undefined)).toBe(created)
        expect(reconcileCreatedKey(created, missing)).toBe(created)
      })

      test('is acknowledged once the server reports its id', () => {
        expect(reconcileCreatedKey(created, waiting)).toEqual({
          ...created,
          acknowledged: true,
        })
      })

      test('is forgotten when an acknowledged key is removed elsewhere', () => {
        const acknowledged = { ...created, acknowledged: true }
        expect(reconcileCreatedKey(acknowledged, waiting)).toBe(acknowledged)
        expect(reconcileCreatedKey(acknowledged, undefined)).toBe(acknowledged)
        expect(reconcileCreatedKey(acknowledged, missing)).toBeNull()
      })

      test('after removal elsewhere the page offers creation again', () => {
        const acknowledged = { ...created, acknowledged: true }
        const next = reconcileCreatedKey(acknowledged, missing)
        expect(
          resolveOnboardingStatus(missing, {
            keyJustCreated: next !== null && !next.acknowledged,
          })
        ).toBe('missing')
      })
    })
  })
})

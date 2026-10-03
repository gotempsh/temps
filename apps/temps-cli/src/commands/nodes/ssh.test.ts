// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  ALL_HOST_KEYS_COMMAND,
  credentialSource,
  describeEnrollment,
  hostKeyCompareCommand,
  hostKeyFileForAlgorithm,
  newLogLines,
  withRetries,
} from './ssh.js'
import type { NodeSshEnrollmentResponse } from '../../api/types.gen.js'

function makeEnrollment(
  overrides: Partial<NodeSshEnrollmentResponse> = {},
): NodeSshEnrollmentResponse {
  return {
    id: 1,
    name: 'worker-1',
    host: 'node.example.com',
    ssh_address: '198.51.100.7:22',
    ssh_user: 'root',
    auth_method: 'password',
    host_key_fingerprint: `SHA256:${'A'.repeat(43)}`,
    pairing_id: 3,
    status: 'running',
    step: 'installing temps',
    log: '',
    error: null,
    agent_mode: null,
    node_id: null,
    created_at: '2026-09-29T10:00:00Z',
    finished_at: null,
    ...overrides,
  }
}

describe('describeEnrollment', () => {
  test('names the step it is on or stopped at', () => {
    expect(describeEnrollment(makeEnrollment())).toBe('running: installing temps')
    expect(describeEnrollment(makeEnrollment({ status: 'failed', step: 'pairing' }))).toBe(
      'failed while pairing',
    )
  })

  test('warns when the agent will not survive a reboot', () => {
    expect(describeEnrollment(makeEnrollment({ status: 'succeeded', agent_mode: 'service' }))).toBe(
      'added',
    )
    expect(
      describeEnrollment(makeEnrollment({ status: 'succeeded', agent_mode: 'detached' })),
    ).toContain('stops at reboot')
  })
})

describe('newLogLines', () => {
  test('returns only complete lines not printed yet', () => {
    const first = newLogLines('a\nb\npart', 0)
    expect(first.lines).toEqual(['a', 'b'])
    const second = newLogLines('a\nb\npartial\nc\n', first.printed)
    expect(second.lines).toEqual(['partial', 'c'])
    expect(newLogLines('a\nb\npartial\nc\n', second.printed).lines).toEqual([])
  })

  test('starts over when the server trimmed the log below what was printed', () => {
    expect(newLogLines('x\n', 50).lines).toEqual(['x'])
  })
})

describe('host key file from the algorithm', () => {
  test('maps each standard key type to its file', () => {
    expect(hostKeyFileForAlgorithm('ssh-ed25519')).toBe('/etc/ssh/ssh_host_ed25519_key.pub')
    expect(hostKeyFileForAlgorithm('ecdsa-sha2-nistp256')).toBe('/etc/ssh/ssh_host_ecdsa_key.pub')
    expect(hostKeyFileForAlgorithm('ecdsa-sha2-nistp521')).toBe('/etc/ssh/ssh_host_ecdsa_key.pub')
    for (const algorithm of ['ssh-rsa', 'rsa-sha2-256', 'rsa-sha2-512']) {
      expect(hostKeyFileForAlgorithm(algorithm)).toBe('/etc/ssh/ssh_host_rsa_key.pub')
    }
  })

  test('falls back to every host key for other types', () => {
    expect(hostKeyFileForAlgorithm('ssh-dss')).toBeNull()
    expect(hostKeyCompareCommand('ssh-dss')).toBe(ALL_HOST_KEYS_COMMAND)
    expect(hostKeyCompareCommand('rsa-sha2-512')).toBe(
      'ssh-keygen -lf /etc/ssh/ssh_host_rsa_key.pub',
    )
  })
})

describe('credentialSource', () => {
  test('prompts for the password only on a terminal', () => {
    expect(credentialSource({}, true)).toEqual({
      method: 'password',
      from: 'prompt',
    })
    expect(() => credentialSource({}, false)).toThrow(/--password-stdin.*--identity-file.*--agent/)
  })

  test('reads the password from piped stdin', () => {
    expect(credentialSource({ passwordStdin: true }, false)).toEqual({
      method: 'password',
      from: 'stdin',
    })
    expect(() => credentialSource({ passwordStdin: true }, true)).toThrow(/stdin is a terminal/)
  })

  test('allows one login method', () => {
    expect(() => credentialSource({ identityFile: 'k', agent: true }, true)).toThrow(/use one of/)
    expect(() => credentialSource({ agent: true, passwordStdin: true }, false)).toThrow(
      /use one of/,
    )
    expect(credentialSource({ agent: true }, false)).toEqual({
      method: 'agent',
    })
  })

  test('takes the passphrase of an identity file from a prompt or stdin', () => {
    expect(credentialSource({ identityFile: 'k' }, false)).toEqual({
      method: 'private_key',
      path: 'k',
      passphrase: 'none',
    })
    expect(credentialSource({ identityFile: 'k', askPassphrase: true }, true)).toMatchObject({
      passphrase: 'prompt',
    })
    expect(credentialSource({ identityFile: 'k', passphraseStdin: true }, false)).toMatchObject({
      passphrase: 'stdin',
    })
    expect(() => credentialSource({ identityFile: 'k', askPassphrase: true }, false)).toThrow(
      /--passphrase-stdin/,
    )
  })

  test('refuses passphrase flags that cannot work', () => {
    expect(() =>
      credentialSource({ identityFile: 'k', askPassphrase: true, passphraseStdin: true }, false),
    ).toThrow(/not both/)
    expect(() => credentialSource({ passwordStdin: true, passphraseStdin: true }, false)).toThrow(
      /both read stdin/,
    )
    expect(() => credentialSource({ askPassphrase: true }, true)).toThrow(
      /--ask-passphrase .*--identity-file/,
    )
    expect(() => credentialSource({ passphraseStdin: true }, false)).toThrow(
      /--passphrase-stdin .*--identity-file/,
    )
  })
})

describe('withRetries', () => {
  const noSleep = async () => {}

  test('retries a failed read until one succeeds', async () => {
    let calls = 0
    const slept: number[] = []
    const value = await withRetries(
      async () => {
        calls++
        if (calls < 3) throw new Error('flaky')
        return 'ok'
      },
      [1, 2, 3],
      async (ms) => {
        slept.push(ms)
      },
    )
    expect(value).toBe('ok')
    expect(slept).toEqual([1, 2])
  })

  test('throws the last error once every retry failed', async () => {
    let calls = 0
    await expect(
      withRetries(
        async () => {
          calls++
          throw new Error(`failure ${calls}`)
        },
        [1, 2],
        noSleep,
      ),
    ).rejects.toThrow('failure 3')
  })
})

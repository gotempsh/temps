// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  ALL_HOST_KEYS_COMMAND,
  hostKeyCompareCommand,
  hostKeyFileForAlgorithm,
  parseSshPort,
} from './ssh-host-key'

describe('host key file from the algorithm', () => {
  test('maps each standard key type to its file', () => {
    expect(hostKeyFileForAlgorithm('ssh-ed25519')).toBe(
      '/etc/ssh/ssh_host_ed25519_key.pub'
    )
    for (const algorithm of [
      'ecdsa-sha2-nistp256',
      'ecdsa-sha2-nistp384',
      'ecdsa-sha2-nistp521',
    ]) {
      expect(hostKeyFileForAlgorithm(algorithm)).toBe(
        '/etc/ssh/ssh_host_ecdsa_key.pub'
      )
    }
    for (const algorithm of ['ssh-rsa', 'rsa-sha2-256', 'rsa-sha2-512']) {
      expect(hostKeyFileForAlgorithm(algorithm)).toBe(
        '/etc/ssh/ssh_host_rsa_key.pub'
      )
    }
  })

  test('ignores case and surrounding space', () => {
    expect(hostKeyFileForAlgorithm(' SSH-ED25519 ')).toBe(
      '/etc/ssh/ssh_host_ed25519_key.pub'
    )
  })

  test('has no file for other key types', () => {
    expect(hostKeyFileForAlgorithm('ssh-dss')).toBeNull()
    expect(hostKeyFileForAlgorithm('sk-ssh-ed25519@openssh.com')).toBeNull()
    expect(hostKeyFileForAlgorithm('')).toBeNull()
  })

  test('the compare command reads that file, or every host key', () => {
    expect(hostKeyCompareCommand('ecdsa-sha2-nistp256')).toBe(
      'ssh-keygen -lf /etc/ssh/ssh_host_ecdsa_key.pub'
    )
    expect(hostKeyCompareCommand('ssh-dss')).toBe(ALL_HOST_KEYS_COMMAND)
    expect(ALL_HOST_KEYS_COMMAND).toBe(
      'for f in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf "$f"; done'
    )
  })
})

describe('parseSshPort', () => {
  test('accepts a port between 1 and 65535', () => {
    expect(parseSshPort('22')).toEqual({ port: 22, error: null })
    expect(parseSshPort(' 2222 ')).toEqual({ port: 2222, error: null })
    expect(parseSshPort('65535')).toEqual({ port: 65535, error: null })
  })

  test('refuses what used to be read as port 22 or passed through', () => {
    expect(parseSshPort('').error).toBe('Enter a port (usually 22)')
    expect(parseSshPort('22abc').error).toBe('The port must be a whole number')
    expect(parseSshPort('-1').error).toBe('The port must be a whole number')
    expect(parseSshPort('0').error).toBe('The port must be between 1 and 65535')
    expect(parseSshPort('70000').error).toBe(
      'The port must be between 1 and 65535'
    )
  })
})

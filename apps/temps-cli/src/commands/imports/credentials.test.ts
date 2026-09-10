// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { resolveCredentials, describeCredentials, tokenLabel } from './credentials.js'

describe('tokenLabel', () => {
  test('calls it an admin password for caprover', () => {
    expect(tokenLabel('caprover')).toBe('admin password')
  })

  test('calls it an admin password for portainer', () => {
    expect(tokenLabel('portainer')).toBe('admin password')
  })

  test('calls it an API token for everything else', () => {
    expect(tokenLabel('coolify')).toBe('API token')
    expect(tokenLabel('dokploy')).toBe('API token')
  })
})

describe('resolveCredentials', () => {
  test('returns empty credentials for docker, which needs nothing', async () => {
    expect(await resolveCredentials('docker', {})).toEqual({})
  })

  test('builds token + base_url credentials from flags without prompting', async () => {
    const creds = await resolveCredentials('coolify', {
      baseUrl: 'https://coolify.example.com',
      token: 'tok_123',
    })
    expect(creds).toEqual({
      token: 'tok_123',
      base_url: 'https://coolify.example.com',
      extra: {},
    })
  })

  test('includes username in extra for portainer, the one source that needs it', async () => {
    const creds = await resolveCredentials('portainer', {
      baseUrl: 'https://portainer.example.com',
      token: 'adminpass',
      username: 'root',
    })
    expect(creds.extra).toEqual({ username: 'root' })
  })

  test('does not require a username for coolify', async () => {
    const creds = await resolveCredentials('coolify', {
      baseUrl: 'https://coolify.example.com',
      token: 'tok_123',
      username: 'should-be-ignored',
    })
    expect(creds.extra).toEqual({})
  })

  describe('kubernetes', () => {
    let dir: string

    beforeEach(() => {
      dir = mkdtempSync(join(tmpdir(), 'temps-cli-kubeconfig-'))
    })

    afterEach(() => {
      rmSync(dir, { recursive: true, force: true })
    })

    test('reads the kubeconfig file content into extra.kubeconfig', async () => {
      const path = join(dir, 'kubeconfig.yaml')
      writeFileSync(path, 'apiVersion: v1\nclusters: []\n')

      const creds = await resolveCredentials('kubernetes', { kubeconfig: path })
      expect(creds).toEqual({ extra: { kubeconfig: 'apiVersion: v1\nclusters: []\n' } })
    })

    test('throws a clear error when the kubeconfig path does not exist', async () => {
      const missing = join(dir, 'does-not-exist.yaml')
      await expect(resolveCredentials('kubernetes', { kubeconfig: missing })).rejects.toThrow(
        /Kubeconfig not found/,
      )
    })

    test('expands a leading ~ to the home directory', async () => {
      const savedHome = process.env.HOME
      process.env.HOME = dir
      try {
        writeFileSync(join(dir, 'kubeconfig.yaml'), 'apiVersion: v1\n')
        const creds = await resolveCredentials('kubernetes', { kubeconfig: '~/kubeconfig.yaml' })
        expect(creds.extra?.kubeconfig).toBe('apiVersion: v1\n')
      } finally {
        process.env.HOME = savedHome
      }
    })
  })

  describe('kamal', () => {
    let dir: string

    beforeEach(() => {
      dir = mkdtempSync(join(tmpdir(), 'temps-cli-deployyml-'))
    })

    afterEach(() => {
      rmSync(dir, { recursive: true, force: true })
    })

    test('reads config/deploy.yml content into extra.deploy_yml', async () => {
      const path = join(dir, 'deploy.yml')
      writeFileSync(path, 'service: myapp\n')

      const creds = await resolveCredentials('kamal', { deployYml: path })
      expect(creds).toEqual({ extra: { deploy_yml: 'service: myapp\n' } })
    })

    test('throws a clear error when deploy.yml does not exist', async () => {
      const missing = join(dir, 'missing-deploy.yml')
      await expect(resolveCredentials('kamal', { deployYml: missing })).rejects.toThrow(
        /deploy\.yml not found/,
      )
    })
  })
})

describe('describeCredentials', () => {
  test('describes a base_url credential without leaking the token', () => {
    const desc = describeCredentials('coolify', {
      token: 'super-secret',
      base_url: 'https://coolify.example.com',
    })
    expect(desc).toContain('https://coolify.example.com')
    expect(desc).not.toContain('super-secret')
  })

  test('describes a kubeconfig credential by byte size, not content', () => {
    const desc = describeCredentials('kubernetes', { extra: { kubeconfig: 'abcde' } })
    expect(desc).toContain('kubeconfig')
    expect(desc).toContain('5 bytes')
  })

  test('describes a deploy.yml credential by byte size, not content', () => {
    const desc = describeCredentials('kamal', { extra: { deploy_yml: '0123456789' } })
    expect(desc).toContain('deploy.yml')
    expect(desc).toContain('10 bytes')
  })

  test('returns null for credential-less sources like docker', () => {
    expect(describeCredentials('docker', {})).toBeNull()
  })
})

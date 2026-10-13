// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { Command } from 'commander'
import { resolveStdinSecret } from '../dns-providers/index.js'
import {
  registerDnsCommands,
  cloudflareCredentials,
  bunnyCredentials,
  route53Credentials,
  digitalOceanCredentials,
  namecheapCredentials,
  gcpCredentials,
  azureCredentials,
} from './index.js'

describe('cloudflareCredentials', () => {
  test('omits account_id when not provided', () => {
    expect(cloudflareCredentials('tok')).toEqual({ type: 'cloudflare', api_token: 'tok' })
  })

  test('includes account_id when provided', () => {
    expect(cloudflareCredentials('tok', 'acct-1')).toEqual({
      type: 'cloudflare',
      api_token: 'tok',
      account_id: 'acct-1',
    })
  })

  test('treats an empty account_id the same as omitted, not an empty field', () => {
    expect(cloudflareCredentials('tok', '')).toEqual({ type: 'cloudflare', api_token: 'tok' })
  })
})

describe('bunnyCredentials', () => {
  test('carries only the api key under the api_key field', () => {
    expect(bunnyCredentials('bunny-key')).toEqual({ type: 'bunny', api_key: 'bunny-key' })
  })
})

describe('route53Credentials', () => {
  test('maps fields to the API snake_case shape', () => {
    expect(route53Credentials('AKIA123', 'secret', 'eu-west-1')).toEqual({
      type: 'route53',
      access_key_id: 'AKIA123',
      secret_access_key: 'secret',
      region: 'eu-west-1',
    })
  })
})

describe('digitalOceanCredentials', () => {
  test('carries only the api token', () => {
    expect(digitalOceanCredentials('do-token')).toEqual({
      type: 'digitalocean',
      api_token: 'do-token',
    })
  })
})

describe('namecheapCredentials', () => {
  test('maps all four required fields', () => {
    expect(namecheapCredentials('user', 'key', 'uname', '1.2.3.4')).toEqual({
      type: 'namecheap',
      api_user: 'user',
      api_key: 'key',
      username: 'uname',
      client_ip: '1.2.3.4',
    })
  })
})

describe('gcpCredentials', () => {
  test('maps service account fields', () => {
    expect(gcpCredentials('proj', 'sa@proj.iam.gserviceaccount.com', 'kid', 'PRIVATE_KEY')).toEqual({
      type: 'gcp',
      project_id: 'proj',
      service_account_email: 'sa@proj.iam.gserviceaccount.com',
      private_key_id: 'kid',
      private_key: 'PRIVATE_KEY',
    })
  })
})

describe('azureCredentials', () => {
  test('maps service principal fields', () => {
    expect(azureCredentials('tenant', 'client', 'secret', 'sub', 'rg')).toEqual({
      type: 'azure',
      tenant_id: 'tenant',
      client_id: 'client',
      client_secret: 'secret',
      subscription_id: 'sub',
      resource_group: 'rg',
    })
  })
})

describe('dns add stdin secret flags', () => {
  const addCommand = () => {
    const program = new Command()
    registerDnsCommands(program)
    const add = program.commands
      .find((command) => command.name() === 'dns')
      ?.commands.find((command) => command.name() === 'add')
    if (!add) {
      throw new Error('dns add command was not registered')
    }
    return add
  }

  test('registers a -stdin twin for every secret flag', () => {
    const flags = addCommand().options.map((option) => option.long)
    for (const flag of [
      '--api-key',
      '--api-token',
      '--secret-access-key',
      '--client-secret',
      '--private-key',
    ]) {
      expect(flags).toContain(flag)
      expect(flags).toContain(`${flag}-stdin`)
    }
  })

  test('the plain --api-key help points at --api-key-stdin', () => {
    const apiKey = addCommand().options.find((option) => option.long === '--api-key')
    expect(apiKey?.description).toContain('prefer --api-key-stdin')
  })

  test('a piped Bunny api key feeds the bunny credentials', async () => {
    const input: { type: string; apiKey?: string; apiKeyStdin?: boolean; yes?: boolean } = {
      type: 'bunny',
      apiKeyStdin: true,
      yes: true,
    }
    const options = await resolveStdinSecret(input, async () => 'bunny-key')
    expect(options.apiKey).toBe('bunny-key')
    expect(bunnyCredentials(options.apiKey ?? '')).toEqual({ type: 'bunny', api_key: 'bunny-key' })
  })

  test('empty stdin is an error rather than a silent empty key', async () => {
    await expect(
      resolveStdinSecret({ type: 'bunny', apiKeyStdin: true }, async () => undefined),
    ).rejects.toThrow('--api-key-stdin was given but no value was piped on stdin')
  })
})

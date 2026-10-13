// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { sandboxUrl, toSandboxView } from './index.js'
import { describeUpgradeRejection, terminalWsUrl } from './shell.js'

describe('toSandboxView', () => {
  // The response types in this module were written against an older, flat
  // contract and never corrected — the 404-ing URL meant nothing ever came
  // back to contradict them. These assertions pin the three fields that
  // were actually wrong.
  const inner = {
    id: 'sbx_abc',
    name: 'ws',
    status: 'running',
    image: 'node:22',
    cwd: '/home/temps/workspace',
    createdAt: 1_700_000_000_000,
    timeout: 3_600_000,
    lifecycle: 'workspace',
  }

  test('maps cwd to work_dir', () => {
    // Not `work_dir` on the wire — reading that gave `undefined`, which the
    // terminal then passed as a PTY cwd.
    expect(toSandboxView(inner).work_dir).toBe('/home/temps/workspace')
  })

  test('derives expires_at from createdAt + timeout', () => {
    // The server sends a duration in ms, not a deadline. There is no
    // `expires_at` field to read.
    const view = toSandboxView(inner)
    expect(view.created_at).toBe('2023-11-14T22:13:20.000Z')
    expect(view.expires_at).toBe('2023-11-14T23:13:20.000Z')
  })

  test('carries lifecycle through for the Kind column', () => {
    expect(toSandboxView(inner).lifecycle).toBe('workspace')
    expect(toSandboxView({ ...inner, lifecycle: undefined }).lifecycle).toBeUndefined()
  })
})

describe('sandboxUrl', () => {
  // This is the regression guard for a real bug: every command in this
  // group built `/v1/sandbox` (singular) while the server has only ever
  // served `/v1/sandboxes`, so they all 404'd against a live instance.
  test('uses the plural route the server actually serves', () => {
    expect(sandboxUrl('https://temps.example.com/api', '')).toBe(
      'https://temps.example.com/api/v1/sandboxes',
    )
    expect(sandboxUrl('https://temps.example.com/api', '/sbx_abc')).toBe(
      'https://temps.example.com/api/v1/sandboxes/sbx_abc',
    )
  })

  test('tolerates trailing slashes on the configured API URL', () => {
    expect(sandboxUrl('https://temps.example.com/api/', '/sbx_abc')).toBe(
      'https://temps.example.com/api/v1/sandboxes/sbx_abc',
    )
    expect(sandboxUrl('https://temps.example.com/api///', '')).toBe(
      'https://temps.example.com/api/v1/sandboxes',
    )
  })
})

describe('terminalWsUrl', () => {
  test('upgrades https to wss', () => {
    const url = terminalWsUrl('https://temps.example.com/api', 'sbx_abc', {})
    expect(url).toBe('wss://temps.example.com/api/v1/sandboxes/sbx_abc/terminal')
  })

  test('upgrades http to ws for local instances', () => {
    const url = terminalWsUrl('http://localhost:8080/api', 'sbx_abc', {})
    expect(url).toBe('ws://localhost:8080/api/v1/sandboxes/sbx_abc/terminal')
  })

  test('appends only the params that were provided', () => {
    const url = terminalWsUrl('https://x.test/api', 'sbx_a', {
      tab: 'main',
      cmd: undefined,
      cols: '120',
      rows: '',
    })
    expect(url).toBe(
      'wss://x.test/api/v1/sandboxes/sbx_a/terminal?tab=main&cols=120',
    )
  })

  test('encodes params so a command with spaces survives', () => {
    const url = terminalWsUrl('https://x.test/api', 'sbx_a', {
      cmd: 'claude --resume',
    })
    expect(url).toContain('cmd=claude%20--resume')
  })

  test('encodes the sandbox id rather than trusting it', () => {
    const url = terminalWsUrl('https://x.test/api', 'sbx a/b', {})
    expect(url).toBe('wss://x.test/api/v1/sandboxes/sbx%20a%2Fb/terminal')
  })
})

describe('describeUpgradeRejection', () => {
  test('surfaces the problem document detail the server sent', () => {
    // The 409 body is the only place the actionable instruction lives; the
    // ws client's own error message carries just the status code.
    const body = JSON.stringify({
      type: 'about:blank',
      title: 'Terminal Unavailable',
      status: 409,
      detail:
        'Could not attach a terminal to sandbox sbx_test: temps-pty-agent is not staged. Run `temps firecracker setup`.',
    })
    const message = describeUpgradeRejection(409, body, 'sbx_test')
    expect(message).toContain('sbx_test')
    expect(message).toContain('Terminal Unavailable')
    expect(message).toContain('temps firecracker setup')
    expect(message).not.toContain('HTTP 409')
  })

  test('falls back to the title when there is no detail', () => {
    const message = describeUpgradeRejection(
      429,
      JSON.stringify({ title: 'Too Many Terminals' }),
      'sbx_test',
    )
    expect(message).toContain('Too Many Terminals')
    expect(message).toContain('HTTP 429')
  })

  test('does not trust a non-problem body', () => {
    // A reverse proxy's HTML error page must not be echoed at the user.
    const message = describeUpgradeRejection(
      502,
      '<html><body>Bad Gateway</body></html>',
      'sbx_test',
    )
    expect(message).toContain('HTTP 502')
    expect(message).toContain('temps sandbox show sbx_test')
    expect(message).not.toContain('<html>')
  })

  test('ignores non-string problem fields', () => {
    const message = describeUpgradeRejection(
      409,
      JSON.stringify({ title: 7, detail: { nested: true } }),
      'sbx_test',
    )
    expect(message).toContain('HTTP 409')
  })

  test('copes with an empty body', () => {
    expect(describeUpgradeRejection(409, '', 'sbx_test')).toContain('HTTP 409')
  })
})

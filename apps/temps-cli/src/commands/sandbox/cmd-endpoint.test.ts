// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { registerSandboxEndpoints } from './endpoints.js'
import { runWithApi } from '../../lib/mock-api.js'

describe('sandbox cmd', () => {
  test('sends the command and prints the NDJSON reply as is', async () => {
    const reply = '{"command":{"cmdId":"c1","exitCode":null}}\n{"command":{"cmdId":"c1","exitCode":0}}\n'
    const run = await runWithApi(
      registerSandboxEndpoints,
      ['sandbox', 'cmd', 'sbx_1', '--command', 'ls', '--args', '-la', '--cwd', '/workspace', '--sudo', '--wait'],
      { 'POST /v1/sandboxes/sbx_1/cmd': new Response(reply, { headers: { 'content-type': 'application/x-ndjson' } }) },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ command: 'ls', args: ['-la'], cwd: '/workspace', sudo: true, wait: true })
  })

  test('--command is required', async () => {
    const run = await runWithApi(registerSandboxEndpoints, ['sandbox', 'cmd', 'sbx_1'], {})
    expect(run.error?.message).toMatch(/Missing required input: --command/)
    expect(run.requests).toEqual([])
  })
})

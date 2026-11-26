// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { readConnectorSecrets, registerTunnelsCommands } from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const dir = mkdtempSync(join(tmpdir(), 'temps-tunnels-test-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

describe('tunnels create', () => {
  test('sends the host suffix and the secrets file, never as arguments', async () => {
    const file = join(dir, 'secrets.json')
    writeFileSync(file, JSON.stringify({ client_secret: 'value-from-file' }))
    const run = await runWithApi(
      registerTunnelsCommands,
      ['tunnels', 'create', '--name', 'lab', '--dns-provider', '2', '--domain', 'example.com', '--node', '4', '--host-suffix', 'lab', '--secrets-file', file, '--json'],
      { 'POST /ingress/connectors': { id: 1, name: 'lab', domain: 'example.com' } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toMatchObject({ host_suffix: 'lab', secrets: { client_secret: 'value-from-file' } })
  })

  test('the secrets must be a JSON object of strings', async () => {
    const file = join(dir, 'bad.json')
    writeFileSync(file, JSON.stringify({ client_secret: 42 }))
    await expect(readConnectorSecrets(file)).rejects.toThrow(/Secret "client_secret" .* must be a string, got number/)
    await expect(readConnectorSecrets('-', (async function* () { yield '{"k":"v"}' })())).resolves.toEqual({ k: 'v' })
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  createProviderKey,
  createShieldToken,
  downloadAutofixPatch,
  listShieldEvents,
  revokeShieldToken,
  streamEvents,
  tailDeploymentJobLogs,
} from '../api/sdk.gen.js'
import { endpoint, flagName, topGroup } from './endpoint.js'
import { runWithApi } from './mock-api.js'
import { createProgram } from '../cli.js'

const dir = mkdtempSync(join(tmpdir(), 'temps-endpoint-test-'))
afterAll(() => rmSync(dir, { recursive: true, force: true }))

function register(program: Command): void {
  const group = topGroup(program, 'demo', 'Demo')
  endpoint(group, createShieldToken, {
    name: 'tokens create',
    description: 'Create a bypass token',
    path: { project_id: 'project', environment_id: 'int' },
    body: { name: { kind: 'string!', help: 'What uses it' }, project_wide: 'bool' },
  })
  endpoint(group, listShieldEvents, {
    name: 'events',
    description: 'Recent Shield events',
    path: { project_id: 'project', environment_id: 'int' },
    query: { limit: 'int', kind: { kind: 'string', values: ['verdict', 'challenge_issued'] }, verdict: 'string', ip: 'string', before: 'string' },
  })
  endpoint(group, revokeShieldToken, {
    name: 'tokens revoke',
    description: 'Revoke a bypass token',
    path: { project_id: 'project', environment_id: 'int', token_id: 'int' },
    confirm: 'This will revoke a bypass token.',
  })
  endpoint(group, createProviderKey, {
    name: 'provider-key',
    description: 'Create provider key',
    body: {
      api_key: 'secret',
      base_url: 'string',
      capabilities: 'json',
      default_model: 'string',
      display_name: 'string!',
      provider: 'string!',
      trust_private_address: 'bool',
    },
  })
  endpoint(group, downloadAutofixPatch, {
    name: 'patch',
    description: 'The fix as a unified diff',
    path: { project_id: 'project', incident_id: 'string' },
    output: 'bytes',
  })
  endpoint(group, streamEvents, {
    name: 'run-events',
    description: 'Stream run events',
    path: { project_id: 'project', run_id: 'int' },
    output: 'events',
  })
}

const project = { 'GET /projects/by-slug/web': { id: 7, slug: 'web', name: 'web' } }

describe('flagName', () => {
  test('snake and camel case become kebab case', () => {
    expect(flagName('retention_days')).toBe('retention-days')
    expect(flagName('networkPolicy')).toBe('network-policy')
    expect(flagName('_runtime')).toBe('runtime')
  })
})

describe('endpoint()', () => {
  test('path parameters are positional, the project comes from -p, and body flags build the body', async () => {
    const run = await runWithApi(register, ['demo', 'tokens', 'create', '3', '-p', 'web', '--name', 'CI', '--project-wide', '--json'], {
      ...project,
      'POST /projects/7/environments/3/shield/tokens': { id: 1, token: 'once' },
    })
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'POST /projects/7/environments/3/shield/tokens', body: { name: 'CI', project_wide: true } }])
    expect(JSON.parse(run.stdout)).toEqual({ id: 1, token: 'once' })
  })

  test('--from-file and --set fill the body; flags override the file', async () => {
    const file = join(dir, 'body.json')
    await Bun.write(file, JSON.stringify({ name: 'from file', project_wide: false }))
    const run = await runWithApi(
      register,
      ['demo', 'tokens', 'create', '3', '-p', '7', '--from-file', file, '--no-project-wide', '--set', 'name="from set"', '--json'],
      { 'POST /projects/7/environments/3/shield/tokens': { id: 1 } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ name: 'from set', project_wide: false })
  })

  test('a required body field that is missing fails before any request', async () => {
    const run = await runWithApi(register, ['demo', 'tokens', 'create', '3', '-p', '7'], {})
    expect(run.error?.message).toMatch(/Missing required input: --name/)
    expect(run.requests).toEqual([])
  })

  test('query flags are typed and enums are checked', async () => {
    const ok = await runWithApi(register, ['demo', 'events', '3', '-p', '7', '--limit', '5', '--kind', 'verdict', '--json'], {
      'GET /projects/7/environments/3/shield/events': { events: [] },
    })
    expect(ok.requests).toEqual(['GET /projects/7/environments/3/shield/events?limit=5&kind=verdict'])
    const badInt = await runWithApi(register, ['demo', 'events', '3', '-p', '7', '--limit', 'many'], {})
    expect(badInt.error?.message).toMatch(/--limit must be a whole number/)
    const badEnum = await runWithApi(register, ['demo', 'events', '3', '-p', '7', '--kind', 'other'], {})
    expect(badEnum.error?.message).toMatch(/--kind must be one of verdict, challenge_issued/)
    const badPath = await runWithApi(register, ['demo', 'events', 'three', '-p', '7'], {})
    expect(badPath.error?.message).toMatch(/<environment-id> must be a whole number/)
    expect([...badInt.requests, ...badEnum.requests, ...badPath.requests]).toEqual([])
  })

  test('a risky operation asks first: refused without a terminal, sent with --yes', async () => {
    const routes = { 'DELETE /projects/7/environments/3/shield/tokens/9': new Response(null, { status: 204 }) }
    const refused = await runWithApi(register, ['demo', 'tokens', 'revoke', '3', '9', '-p', '7'], routes, { stdin: '' })
    expect(refused.error?.message).toMatch(/not an interactive terminal/)
    expect(refused.requests).toEqual([])
    const done = await runWithApi(register, ['demo', 'tokens', 'revoke', '3', '9', '-p', '7', '--yes'], routes)
    expect(done.error).toBeUndefined()
    expect(done.requests).toEqual(['DELETE /projects/7/environments/3/shield/tokens/9'])
  })

  test('a secret is read from stdin, never from an argument', async () => {
    const run = await runWithApi(
      register,
      ['demo', 'provider-key', '--provider', 'openai', '--display-name', 'main', '--api-key-stdin', '--capabilities', '{"vision":true}', '--json'],
      { 'POST /ai/providers': { id: 1, api_key: '***' } },
      { stdin: 'sk-test\n' },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ provider: 'openai', display_name: 'main', api_key: 'sk-test', capabilities: { vision: true } })
    const help = createHelp(register, ['demo', 'provider-key'])
    expect(help).toContain('--api-key-stdin')
    expect(help).not.toMatch(/--api-key </)
  })

  test('the API error names the operation', async () => {
    const run = await runWithApi(register, ['demo', 'events', '3', '-p', '7'], {
      'GET /projects/7/environments/3/shield/events': Response.json({ title: 'Not Found', detail: 'environment 3 not found' }, { status: 404 }),
    })
    expect(run.error?.message).toMatch(/Could not recent Shield events: .*environment 3 not found/)
  })

  test('a download is written to -o', async () => {
    const out = join(dir, 'fix.diff')
    const run = await runWithApi(register, ['demo', 'patch', 'inc_1', '-p', '7', '-o', out], {
      'GET /projects/7/autofix/incidents/inc_1/patch': new Response('--- a\n+++ b\n', { headers: { 'content-type': 'text/x-diff' } }),
    })
    expect(run.error).toBeUndefined()
    expect(readFileSync(out, 'utf8')).toBe('--- a\n+++ b\n')
  })

  test('server-sent events print one JSON line each', async () => {
    const run = await runWithApi(register, ['demo', 'run-events', '4', '-p', '7'], {
      'GET /projects/7/autofixer/runs/4/stream': new Response('event: log\ndata: {"line":"one"}\n\nevent: done\ndata: {"ok":true}\n\n', {
        headers: { 'content-type': 'text/event-stream' },
      }),
    })
    expect(run.error).toBeUndefined()
    expect(run.stdout.split('\n').map((line) => JSON.parse(line))).toEqual([
      { event: 'log', data: { line: 'one' } },
      { event: 'done', data: { ok: true } },
    ])
  })

  test('registering the same command twice fails at startup', () => {
    const program = new Command()
    register(program)
    expect(() => register(program)).toThrow(/registered twice/)
  })
})

describe('WebSocket streams', () => {
  test('a socket stream prints each message on its own line', async () => {
    const server = Bun.serve<string, never>({
      port: 0,
      fetch(req, srv) {
        const ok = req.headers.get('authorization') === 'Bearer test-token-never-sent'
        if (ok && srv.upgrade(req, { data: new URL(req.url).pathname })) return undefined
        return new Response('no', { status: 401 })
      },
      websocket: {
        open(ws) {
          ws.send(`path ${ws.data}`)
          ws.send('second line\n')
          ws.close(1000)
        },
        message() {},
      },
    })
    const register = (program: Command) => {
      endpoint(topGroup(program, 'demo', 'Demo'), tailDeploymentJobLogs, {
        name: 'tail',
        description: 'Tail job logs',
        path: { project_id: 'project', deployment_id: 'int', job_id: 'string' },
        output: 'socket',
      })
    }
    try {
      const run = await runWithApi(register, ['demo', 'tail', '5', 'build', '-p', '7'], {}, { apiUrl: `http://127.0.0.1:${server.port}` })
      expect(run.error).toBeUndefined()
      expect(run.stdout.split('\n')).toEqual(['path /api/projects/7/deployments/5/jobs/build/logs/tail', 'second line'])
    } finally {
      server.stop(true)
    }
  })
})

describe('the whole CLI', () => {
  test('every table registers: no duplicate commands or flags, and every endpoint command has --json', () => {
    const program = createProgram()
    const walk = (command: Command): Command[] => command.commands.flatMap((c) => [c, ...walk(c)])
    const leaves = walk(program).filter((c) => c.commands.length === 0)
    expect(leaves.length).toBeGreaterThan(1500)
    for (const name of ['shield', 'vault', 'agents']) {
      const group = program.commands.find((c) => c.name() === name)
      expect(group).toBeDefined()
      for (const leaf of walk(group as Command).filter((c) => c.commands.length === 0)) {
        expect(leaf.options.some((o) => o.long === '--json')).toBe(true)
      }
    }
  })
})

function createHelp(registerFn: (program: Command) => void, path: string[]): string {
  const program = new Command()
  registerFn(program)
  let command: Command = program
  for (const word of path) command = command.commands.find((c) => c.name() === word) as Command
  return command.helpInformation()
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, test, expect } from 'bun:test'
import { runWithApi } from '../../lib/mock-api.js'
import { Command } from 'commander'
import { mkdtemp, readFile, rm, symlink } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { integer, positive, payload, registerManagement, diagnose } from './manage.js'
import { scaffoldWorker, workerFiles } from './init.js'
import type { WorkflowRunResponse } from '../../api/types.gen.js'

describe('workflow management', () => {
  test('validates scope, sequences and bounded JSON before any request', () => {
    expect(integer('0')).toBe(0)
    for (const bad of ['-1', '1.5', '2147483648', 'NaN']) expect(() => integer(bad)).toThrow()
    expect(() => positive('0')).toThrow()
    expect(payload('{"id":1}')).toEqual({ id: 1 })
    expect(() => payload('{bad')).toThrow('valid JSON')
    expect(() => payload(' '.repeat(4 * 1024 * 1024 + 1))).toThrow('4 MiB')
  })
  test('all lifecycle operations are discoverable and mutations fail before auth without confirmation', async () => {
    const parent = new Command('workflows'); registerManagement(parent)
    expect(parent.commands.map(c => c.name())).toEqual(['list', 'runs', 'get', 'diagnose', 'start', 'cancel', 'resume', 'retry', 'delete', 'send-event', 'watch'])
    for (const action of ['cancel', 'resume', 'retry', 'delete']) {
      await expect(parent.parseAsync([action, 'run', '--project-id', '1', '--environment-id', '2'], { from: 'user' })).rejects.toThrow('--yes')
    }
  })
  test('diagnosis uses metadata, not application payloads', () => {
    const result = diagnose({ id: 'run', status: 'waiting', awaited_event: { name: 'approval' }, input: 'private', output: 'private' } as unknown as WorkflowRunResponse)
    expect(result.next_action).toContain('matching key')
    expect(JSON.stringify(result)).not.toContain('private')
  })
  test('routes scoped lifecycle requests and explicit reveals through the generated client', async () => {
    const base = '/projects/1/environments/2/workflows'
    const flags = ['--project-id', '1', '--environment-id', '2', '--json']
    const cases: Array<[string[], string, unknown, unknown?]> = [
      [['list'], `GET ${base}`, { workflows: [] }],
      [['runs', '--page', '2'], `GET ${base}/runs`, { runs: [] }],
      [['get', 'run'], `GET ${base}/runs/run`, { id: 'run', status: 'completed' }],
      [['watch', 'run'], `GET ${base}/runs/run`, { id: 'run', status: 'completed' }],
      [['get', 'run', '--reveal', 'step_error', '--sequence', '0'], `POST ${base}/runs/run/reveal`, { value: 'untrusted' }, { target: 'step_error', sequence: 0 }],
      [['start', 'hello', '--idempotency-key', 'once', '--input', '{"x":1}'], `POST ${base}/runs`, { created: true }, { workflow: 'hello', input: { x: 1 }, idempotency_key: 'once' }],
      [['retry', 'run', '--yes', '--from-step', '0'], `POST ${base}/runs/run/retry`, { id: 'run' }, { from_sequence: 0 }],
      [['cancel', 'run', '--yes'], `POST ${base}/runs/run/cancel`, { id: 'run' }],
      [['resume', 'run', '--yes'], `POST ${base}/runs/run/resume`, { id: 'run' }],
      [['delete', 'run', '--yes'], `DELETE ${base}/runs/run`, { deleted: true }],
      [['send-event', 'approved', '--key', 'order', '--payload', '{"ok":true}'], `POST ${base}/events`, { delivered: 1 }, { name: 'approved', key: 'order', payload: { ok: true } }],
    ]
    for (const [args, route, response, body] of cases) {
      const result = await runWithApi(registerManagement, [...args, ...flags], { [route]: response })
      expect(result.error).toBeUndefined()
      expect(result.requests[0]?.split('?')[0]).toBe(route)
      if (body) expect(result.bodies[0]?.body).toEqual(body)
      expect(() => JSON.parse(result.stdout)).not.toThrow()
      if (args.includes('--reveal')) expect(JSON.parse(result.stdout).untrusted_payload).toEqual(response)
    }
    const denied = await runWithApi(registerManagement, ['get', 'run', ...flags], { [`GET ${base}/runs/run`]: Response.json({ title: 'Forbidden' }, { status: 403 }) })
    expect(denied.error).toBeDefined()
    expect(denied.stdout).toBe('')
  })
  test('init creates an isolated worker and refuses existing paths and symlinks', async () => {
    const root = await mkdtemp(join(tmpdir(), 'workflow-init-'))
    try {
      const target = join(root, 'worker'); await scaffoldWorker(target)
      expect(JSON.parse(await readFile(join(target, 'package.json'), 'utf8')).dependencies['@temps-sdk/workflows']).toBe('0.1.0')
      expect(await readFile(join(target, 'worker.mjs'), 'utf8')).toContain('createWorkflowHandler')
      expect(workerFiles.Dockerfile).toContain('USER node')
      await expect(scaffoldWorker(target)).rejects.toThrow()
      await symlink(target, join(root, 'alias'))
      await expect(scaffoldWorker(join(root, 'alias'))).rejects.toThrow()
      expect(await readFile(join(target, 'worker.mjs'), 'utf8')).toEqual(workerFiles['worker.mjs']!)
    } finally { await rm(root, { recursive: true, force: true }) }
  })
})

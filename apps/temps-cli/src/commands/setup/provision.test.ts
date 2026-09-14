// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, test, expect } from 'bun:test'
import { mkdtemp, writeFile, readFile, stat, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { installScript, parseResult, provision, remoteCommand, validateOptions, type SetupOptions, type SetupDependencies } from './provision.js'
import { setupTelemetry } from './telemetry.js'
import { verifySetup } from './index.js'

const options: SetupOptions = { ssh: 'root@server.example', email: 'admin@example.com', port: '22', context: 'test', channel: 'stable' }
const raw = JSON.stringify({ status: 'ok', console_url: 'https://console.example.com', api_key: 'test-key-12345', admin_email: 'admin@example.com', admin_password: 'never-display-this' })

describe('SSH setup boundary', () => {
  test('rejects SSH options, shell syntax, invalid ports and installer flags', () => {
    for (const ssh of ['-oProxyCommand=bad', 'root@host;id', '$(id)', 'host\nother', 'ssh://host']) {
      expect(() => validateOptions({ ...options, ssh })).toThrow()
    }
    for (const port of ['0', '65536', '22;id', '22.5']) expect(() => validateOptions({ ...options, port })).toThrow()
    expect(() => validateOptions({ ...options, runtimeVersion: '--help' })).toThrow()
    expect(() => validateOptions({ ...options, email: 'a@example.com\ncommand' })).toThrow()
  })
  test('preserves SSH key path as one argument and requires trusted hosts', () => {
    const args = remoteCommand({ ...options, identity: '/tmp/a key $(id)' })
    expect(args).toContain('/tmp/a key $(id)')
    expect(args).toContain('StrictHostKeyChecking=yes')
    expect(args).toContain('ForwardAgent=no')
    expect(args).toContain('BatchMode=yes')
  })
  test('result drops the password and rejects plaintext or credential-bearing URLs', () => {
    expect(parseResult(raw)).toEqual({ url: 'https://console.example.com', apiKey: 'test-key-12345', email: 'admin@example.com' })
    for (const url of ['http://console.example.com', 'https://user:pass@console.example.com', 'https://console.example.com/?token=secret']) {
      expect(() => parseResult(JSON.stringify({ ...JSON.parse(raw), console_url: url }))).toThrow()
    }
    expect(() => parseResult('secret invalid response')).toThrow('Installer did not return')
    expect(() => parseResult(JSON.stringify({ ...JSON.parse(raw), api_key: null }))).toThrow()
  })
  test('completed remote setup reuses result without downloading or reinstalling', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-setup-retry-'))
    try {
      await writeFile(join(dir, 'setup-result.json'), raw, { mode: 0o600 })
      // Execute the actual generated script with only the fixed root path and
      // Linux lock primitive adapted for this isolated macOS fixture.
      const script = 'flock() { return 0; }\ncurl() { exit 91; }\n' + installScript(options).replaceAll('/root/.temps', dir)
      const result = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(result.status).toBe(0)
      expect(result.stdout).toBe(raw)
    } finally { await rm(dir, { recursive: true, force: true }) }
  })
  test('download failure cannot execute a partial installer', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-setup-failure-'))
    try {
      const script = 'flock() { return 0; }\ncurl() { return 22; }\n' + installScript(options).replaceAll('/root/.temps', dir)
      const result = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(result.status).toBe(22)
      expect(result.stdout).toBe('')
    } finally { await rm(dir, { recursive: true, force: true }) }
  })
  test('fresh install keeps credential-bearing output in a protected remote log', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-setup-fresh-'))
    try {
      const fixture = join(dir, 'fixture.sh')
      await writeFile(fixture, `echo private-installer-password\nprintf '%s' '${raw}' > '${dir}/setup-result.json'\n`)
      const script = `flock() { return 0; }\ncurl() { cp '${fixture}' "\${@: -1}"; }\n` + installScript(options).replaceAll('/root/.temps', dir)
      const result = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(result.status).toBe(0)
      expect(result.stdout).toBe(raw)
      expect(result.stderr).toBe('')
      expect(await readFile(join(dir, 'cli-setup.log'), 'utf8')).toContain('private-installer-password')
      expect((await stat(join(dir, 'cli-setup.log'))).mode & 0o777).toBe(0o600)
    } finally { await rm(dir, { recursive: true, force: true }) }
  })
})

describe('setup workflow', () => {
  function fixture(fail?: string) {
    const calls: string[] = []
    const deps: SetupDependencies = {
      remote: async (_, step) => { calls.push(step); if (fail === step) throw new Error('private remote error'); return step === 'install' ? raw : 'ready' },
      verify: async () => { calls.push('verify'); if (fail === 'verify') throw new Error('secret key') },
      save: async () => { calls.push('save'); if (fail === 'context') throw new Error('disk full') },
      event: (step, status) => { calls.push(`${step}:${status}`) },
    }
    return { deps, calls }
  }
  test('verifies before saving context', async () => {
    const { deps, calls } = fixture()
    await provision(options, deps)
    expect(calls.indexOf('verify')).toBeLessThan(calls.indexOf('save'))
    expect(calls.at(-1)).toBe('context:completed')
  })
  test('failed verification leaves local credentials unchanged and redacts errors', async () => {
    const { deps, calls } = fixture('verify')
    await expect(provision(options, deps)).rejects.toThrow('Setup verify failed')
    expect(calls).not.toContain('save')
    expect(calls.at(-1)).toBe('verify:failed')
  })
  test('failed installation stops verification and records failure', async () => {
    const { deps, calls } = fixture('install')
    await expect(provision(options, deps)).rejects.toThrow('Setup install failed')
    expect(calls).not.toContain('verify')
    expect(calls).not.toContain('save')
  })
  test('failed context write is not reported as success', async () => {
    const { deps, calls } = fixture('context')
    await expect(provision(options, deps)).rejects.toThrow('Setup context failed')
    expect(calls.at(-1)).toBe('context:failed')
  })
})

describe('setup analytics', () => {
  test('no consent means no requests', async () => {
    let requests = 0
    const telemetry = setupTelemetry(false, '0.1.0', (async () => { requests++; return new Response() }) as unknown as typeof fetch)
    telemetry.record('install', 'started')
    await telemetry.flush()
    expect(requests).toBe(0)
  })
  test('emits only coarse allowlisted fields, with one ID per attempt', async () => {
    let body = ''
    const telemetry = setupTelemetry(true, '0.1.0', (async (_, init) => { body = String(init?.body); return new Response() }) as typeof fetch)
    telemetry.record('install', 'started')
    telemetry.record('install', 'completed')
    await telemetry.flush()
    const events = JSON.parse(body).events
    expect(events[0].anonymous_id).toBe(events[1].anonymous_id)
    expect(Object.keys(events[0].properties).sort()).toEqual(['cli_version', 'elapsed_bucket', 'method', 'status', 'step'])
    expect(body).not.toContain(options.ssh)
    expect(body).not.toContain(options.email)
  })
  test('collector failures cannot fail setup', async () => {
    const telemetry = setupTelemetry(true, '0.1.0', (async () => { throw new Error('offline') }) as unknown as typeof fetch)
    telemetry.record('verify', 'failed')
    await expect(telemetry.flush()).resolves.toBeUndefined()
  })
})

describe('authenticated readiness', () => {
  test('uses the bootstrap key on the generated user endpoint without redirects', async () => {
    let request: Request | undefined
    await verifySetup(parseResult(raw), (async (input: Request) => {
      request = input
      return Response.json({ id: 1, email: options.email })
    }) as unknown as typeof fetch)
    expect(request?.url).toBe('https://console.example.com/api/user/me')
    expect(request?.headers.get('Authorization')).toBe('Bearer test-key-12345')
    expect(request?.redirect).toBe('error')
  })
  test('rejects invalid credentials without surfacing a private response', async () => {
    await expect(verifySetup(parseResult(raw), (async () => Response.json({ secret: 'do-not-log' }, { status: 401 })) as unknown as typeof fetch)).rejects.toThrow('installer API key was rejected')
  })
})

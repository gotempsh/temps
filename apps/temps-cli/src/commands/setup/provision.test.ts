// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, test, expect } from 'bun:test'
import { mkdtemp, chmod, stat, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawnSync } from 'node:child_process'
import { INSPECT_RESULT_SCRIPT, installScript, parseResult, provision, remoteCommand, validateOptions, type SetupOptions, type SetupDependencies } from './provision.js'
import { setupTelemetry } from './telemetry.js'
import { verifySetup, assertSetupContext, refreshedContext } from './index.js'

const options: SetupOptions = { ssh: 'root@server.example', email: 'admin@example.com', port: '22', context: 'test', channel: 'stable' }
const raw = JSON.stringify({ status: 'ok', mode: 'quick', channel: 'stable', console_url: 'https://console.example.com', api_key: 'test-key-12345', admin_email: 'admin@example.com', admin_password: 'never-display-this' })

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
      await Bun.write(fixture, `echo private-installer-password\nprintf '%s' '${raw}' > '${dir}/setup-result.json'\n`)
      const script = `flock() { return 0; }\ncurl() { cp '${fixture}' "\${@: -1}"; }\n` + installScript(options).replaceAll('/root/.temps', dir)
      const result = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(result.status).toBe(0)
      expect(result.stdout).toBe(raw)
      expect(result.stderr).toBe('')
      expect(await Bun.file(join(dir, 'cli-setup.log')).text()).toContain('private-installer-password')
      expect((await stat(join(dir, 'cli-setup.log'))).mode & 0o777).toBe(0o600)
    } finally { await rm(dir, { recursive: true, force: true }) }
  })
})

describe('setup workflow', () => {
  function fixture(fail?: string) {
    const calls: string[] = []
    const deps: SetupDependencies = {
      remote: async (script, step) => { calls.push(step); if (fail === step) throw new Error('private remote error'); return script === INSPECT_RESULT_SCRIPT ? '\nabsent\n' : step === 'install' ? raw : 'ready' },
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


describe('setup context refresh', () => {
  const result = parseResult(raw)
  const current = { name: 'test', ...result, isActive: false, defaultProject: 'important-project', keyPrefix: 'test-key', expiresAt: '2027-01-01' }
  test('retains project, active state and metadata for an unchanged key', () => {
    expect(refreshedContext('test', current, result)).toEqual(current)
  })
  test('key rotation clears stale key metadata but preserves preferences', () => {
    expect(refreshedContext('test', current, { ...result, apiKey: 'new-key-123' })).toEqual({ ...current, apiKey: 'new-key-123', keyPrefix: undefined, expiresAt: undefined })
  })
  test('rejects occupied names for fresh or different servers before install', () => {
    expect(() => assertSetupContext('test', current, undefined)).toThrow('cannot be matched')
    expect(() => assertSetupContext('test', current, { ...result, url: 'https://another.example' })).toThrow('cannot be matched')
    expect(() => assertSetupContext('test', { ...current, url: result.url + '/api/' }, result)).not.toThrow()
    expect(() => assertSetupContext('new', null, undefined)).not.toThrow()
  })
  test('reports authenticated identity mismatch immediately', async () => {
    let calls = 0
    await expect(verifySetup(result, (async () => { calls++; return Response.json({ email: 'other@example.com' }) }) as unknown as typeof fetch)).rejects.toThrow('does not match the installer admin email')
    expect(calls).toBe(1)
  })
})


describe('retry inspection', () => {
  function workflow(inspection: string, beforeInstall?: SetupDependencies['beforeInstall']) {
    const calls: string[] = []
    return { calls, deps: {
      remote: async (script: string, step: string) => { calls.push(step); return script === INSPECT_RESULT_SCRIPT ? inspection : step === 'install' ? raw : 'ready' },
      beforeInstall,
      verify: async () => {}, save: async () => {}, event: () => {},
    } }
  }
  test('valid matching selection reuses completion without installing', async () => {
    const { deps, calls } = workflow('quick:stable:latest\n123 456\n' + raw)
    await provision(options, deps)
    expect(calls).not.toContain('install')
  })
  test('incomplete CLI result resumes installer', async () => {
    for (const value of ['', '{broken', '{}', JSON.stringify({ ...JSON.parse(raw), api_key: null })]) {
      const { deps, calls } = workflow('quick:stable:latest\n123 456\n' + value)
      await provision(options, deps)
      expect(calls).toContain('install')
    }
  })
  test('context guard runs before mutation on fresh servers', async () => {
    const { deps, calls } = workflow('\nabsent\n', async result => assertSetupContext('test', { name: 'test', ...parseResult(raw) }, result))
    await expect(provision(options, deps)).rejects.toThrow('cannot be matched')
    expect(calls).not.toContain('install')
  })
  test('rejects changed selection and unknown legacy pins before installing', async () => {
    for (const inspection of ['quick:beta:latest\n123 456\n' + raw, 'quick:stable:v0.1.0\n123 456\n' + raw, '\n123 456\n' + JSON.stringify({ ...JSON.parse(raw), mode: 'advanced' })]) {
      const { deps, calls } = workflow(inspection)
      await expect(provision(options, deps)).rejects.toThrow()
      expect(calls).not.toContain('install')
    }
    const { deps, calls } = workflow('\n123 456\n' + raw)
    await expect(provision({ ...options, runtimeVersion: 'v0.1.0' }, deps)).rejects.toThrow('Cannot establish')
    expect(calls).not.toContain('install')
  })
  test('actual shell preserves broken result and refuses a stale snapshot', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'temps-setup-repair-'))
    try {
      const broken = '{interrupted'
      await Bun.write(join(dir, 'setup-result.json'), broken)
      await chmod(join(dir, 'setup-result.json'), 0o600)
      await Bun.write(join(dir, 'cli-setup-selection'), 'quick:stable:latest')
      const inspection = spawnSync('bash', ['-s'], { input: INSPECT_RESULT_SCRIPT.replaceAll('/root/.temps', dir), encoding: 'utf8' })
      const snapshot = inspection.stdout.split('\n')[1]!
      const fixture = join(dir, 'fixture.sh')
      await Bun.write(fixture, `printf '%s' '${raw}' > '${dir}/setup-result.json'\n`)
      const script = `flock() { return 0; }\ncurl() { cp '${fixture}' "\${@: -1}"; }\n` + installScript(options, snapshot).replaceAll('/root/.temps', dir)
      const result = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(result.status).toBe(0)
      expect(result.stdout).toBe(raw)
      expect(await Bun.file(join(dir, 'setup-result.incomplete.json')).text()).toBe(broken)
      const changed = spawnSync('bash', ['-s'], { input: script, encoding: 'utf8' })
      expect(changed.status).toBe(21)
    } finally { await rm(dir, { recursive: true, force: true }) }
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readFileSync, statSync } from 'node:fs'
import { mkdir, readdir, rm, utimes, writeFile } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import { createServer as createHttpServer } from 'node:http'
import { createServer, type AddressInfo } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { localPaths, type LocalState } from './runtime.js'
import {
  acquireLock,
  destroySandboxes,
  findBundledSkill,
  installSkill,
  isLocalServer,
  isPortFree,
  pickPort,
  processAlive,
  readState,
  removeWorkloads,
  stopServer,
  tailFile,
  withLock,
  writeState,
} from './steps.js'

const dirs: string[] = []
function tempDir(): string {
  const dir = mkdtempSync(join(tmpdir(), 'temps-start-'))
  dirs.push(dir)
  return dir
}

afterEach(async () => {
  await Promise.all(dirs.splice(0).map((dir) => rm(dir, { recursive: true, force: true })))
})

const STATE: LocalState = {
  version: 'v0.1.0-beta.56',
  dbPort: 55432,
  dbPassword: 'db-secret',
  proxyPort: 8080,
  consolePort: 8700,
  adminEmail: 'admin@localhost',
  adminPassword: 'Tl-secret-9a',
  setupComplete: true,
}

describe('state file', () => {
  test('round-trips and is readable only by its owner', async () => {
    const paths = localPaths(tempDir())
    expect(await readState(paths)).toBeNull()
    await writeState(paths, STATE)
    expect(await readState(paths)).toEqual(STATE)
    expect(statSync(paths.state).mode & 0o777).toBe(0o600)
  })

  test('a corrupt file explains how to start fresh', async () => {
    const paths = localPaths(tempDir())
    await mkdir(paths.root, { recursive: true })
    await writeFile(paths.state, '{not json')
    await expect(readState(paths)).rejects.toThrow(/temps stop --delete-data/)
  })
})

describe('the Temps skill', () => {
  test('is found from the source tree', () => {
    const found = findBundledSkill()
    expect(found).not.toBeNull()
    expect((found as string).endsWith(join('skills', 'temps'))).toBe(true)
    expect(existsSync(join(found as string, 'SKILL.md'))).toBe(true)
  })

  test('is not found where there is no skills/temps folder', async () => {
    const dir = tempDir()
    // A neighbouring skill is not a substitute for it.
    await mkdir(join(dir, 'skills', 'deploy-to-temps'), { recursive: true })
    await writeFile(join(dir, 'skills', 'deploy-to-temps', 'SKILL.md'), '# other')
    expect(findBundledSkill(join(dir, 'index.js'))).toBeNull()
  })

  test('is copied into each agent folder, replaces its old copy and touches no other skill', async () => {
    const source = join(tempDir(), 'skills', 'temps')
    await mkdir(join(source, 'references'), { recursive: true })
    await writeFile(join(source, 'SKILL.md'), '# new')
    await writeFile(join(source, 'references', 'a.md'), 'ref')

    const base = tempDir()
    const stale = join(base, '.claude', 'skills', 'temps')
    const neighbour = join(base, '.claude', 'skills', 'my-own-skill')
    await mkdir(stale, { recursive: true })
    await mkdir(neighbour, { recursive: true })
    await writeFile(join(stale, 'old.md'), 'stale')
    await writeFile(join(neighbour, 'SKILL.md'), '# mine')

    const targets = await installSkill(source, base)
    expect(targets).toEqual([join(base, '.claude', 'skills'), join(base, '.agents', 'skills')])
    for (const target of targets) {
      expect(readFileSync(join(target, 'temps', 'SKILL.md'), 'utf-8')).toBe('# new')
      expect(existsSync(join(target, 'temps', 'references', 'a.md'))).toBe(true)
    }
    expect(existsSync(join(stale, 'old.md'))).toBe(false)
    expect(readFileSync(join(neighbour, 'SKILL.md'), 'utf-8')).toBe('# mine')
    // Exactly one skill was installed.
    expect(await readdir(join(base, '.agents', 'skills'))).toEqual(['temps'])
  })
})

describe('ports and processes', () => {
  test('skips a port another program holds', async () => {
    const server = createServer()
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const busy = (server.address() as { port: number }).port
    try {
      expect(await isPortFree(busy)).toBe(false)
      const picked = await pickPort([busy, busy + 1, busy + 2], [busy + 1], 'test')
      expect(picked).toBe(busy + 2)
    } finally {
      server.close()
    }
  })

  test('fails with the range it tried when nothing is free', async () => {
    await expect(pickPort([1, 2], [1, 2], 'console API')).rejects.toThrow(/console API.*1–2/)
  })

  test('knows whether a pid is alive', () => {
    expect(processAlive(process.pid)).toBe(true)
    expect(processAlive(undefined)).toBe(false)
    expect(processAlive(2 ** 22 + 12345)).toBe(false)
  })

  test('never signals a live pid that is not the local server', async () => {
    // This test runner is alive but is not `~/.temps/local/bin/<v>/temps serve`.
    const paths = localPaths(tempDir())
    expect(await isLocalServer(process.pid, paths)).toBe(false)
    expect(await stopServer(process.pid, paths)).toBe(false)
    expect(await stopServer(undefined, paths)).toBe(false)
    expect(processAlive(process.pid)).toBe(true)
  })
})

describe('state file validation', () => {
  test('rejects unparseable and wrongly shaped state with the reset hint', async () => {
    const paths = localPaths(tempDir())
    await mkdir(paths.root, { recursive: true })
    await writeFile(paths.state, '{ not json')
    await expect(readState(paths)).rejects.toThrow(/Could not read .*state\.json.*temps stop --delete-data/)
    await writeFile(paths.state, JSON.stringify({ version: 'v1', pid: 'abc' }))
    await expect(readState(paths)).rejects.toThrow(/not a valid state file.*temps stop --delete-data/)
  })
})

describe('start/stop lock', () => {
  const claims = async (paths: ReturnType<typeof localPaths>) => (await readdir(paths.lock)).sort()
  const writeClaim = async (paths: ReturnType<typeof localPaths>, name: string, entry: unknown) => {
    await mkdir(paths.lock, { recursive: true })
    await writeFile(join(paths.lock, name), typeof entry === 'string' ? entry : JSON.stringify(entry))
  }
  const DEAD_PID = 2 ** 22 + 12345

  test('a second command fails while the first holds the lock, and succeeds after release', async () => {
    const paths = localPaths(tempDir())
    const release = await acquireLock(paths, 'start')
    expect(await claims(paths)).toHaveLength(1)
    await expect(acquireLock(paths, 'stop')).rejects.toThrow(new RegExp(`temps start.*pid ${process.pid}.*already working`))
    // The loser withdrew its claim and left the holder's alone.
    expect(await claims(paths)).toHaveLength(1)
    await release()
    expect(await claims(paths)).toEqual([])
    await (await acquireLock(paths, 'stop'))()
    expect(await claims(paths)).toEqual([])
  })

  test('overlapping callers never run together', async () => {
    const paths = localPaths(tempDir())
    let inside = 0
    let peak = 0
    let ran = 0
    const attempt = () =>
      withLock(paths, 'start', async () => {
        inside += 1
        peak = Math.max(peak, inside)
        await new Promise((resolve) => setTimeout(resolve, 40))
        inside -= 1
        ran += 1
      })
    for (let round = 0; round < 5; round++) {
      await Promise.allSettled(Array.from({ length: 8 }, attempt))
    }
    expect(peak).toBe(1)
    expect(ran).toBeGreaterThanOrEqual(5)
    expect(await claims(paths)).toEqual([])
  }, 30_000)

  test('releases the lock when the work throws', async () => {
    const paths = localPaths(tempDir())
    await expect(withLock(paths, 'start', async () => Promise.reject(new Error('boom')))).rejects.toThrow('boom')
    expect(await claims(paths)).toEqual([])
  })

  test('skips and removes a claim whose process is gone', async () => {
    const paths = localPaths(tempDir())
    await writeClaim(paths, `${DEAD_PID}-aaaa.json`, { pid: DEAD_PID, command: 'start', startedAt: Date.now(), processStart: 'x' })
    const release = await acquireLock(paths, 'stop')
    expect(await claims(paths)).toHaveLength(1)
    expect((await claims(paths))[0]).toMatch(new RegExp(`^${process.pid}-`))
    await release()
  })

  test('a claim whose pid now belongs to another process is not a holder', async () => {
    const paths = localPaths(tempDir())
    // This pid is alive, but it did not start at the recorded time: the
    // writer died and the pid was reused.
    await writeClaim(paths, `${process.pid}-bbbb.json`, {
      pid: process.pid,
      command: 'start',
      startedAt: Date.now() - 86_400_000,
      processStart: 'Thu Jan  1 00:00:00 1970',
    })
    await (await acquireLock(paths, 'stop'))()
    expect(await claims(paths)).toEqual([])
  })

  test('when a live writer cannot be ruled out, the lock stays held', async () => {
    const paths = localPaths(tempDir())
    // Alive pid, no recorded start time to compare: assume it is the writer.
    await writeClaim(paths, `${process.pid}-cccc.json`, { pid: process.pid, command: 'start', startedAt: Date.now(), processStart: null })
    await expect(acquireLock(paths, 'stop')).rejects.toThrow(/already working/)
    expect(await claims(paths)).toEqual([`${process.pid}-cccc.json`])
  })

  test('clears litter: unparseable claims and drafts of dead writers, but not a live draft', async () => {
    const paths = localPaths(tempDir())
    await writeClaim(paths, 'garbage.json', '{ not json')
    await writeClaim(paths, `${DEAD_PID}-dddd.tmp`, 'half')
    await writeClaim(paths, `${process.pid}-eeee.tmp`, 'half')
    const release = await acquireLock(paths, 'start')
    await release()
    expect(await claims(paths)).toEqual([`${process.pid}-eeee.tmp`])
  })
})

describe('tailFile', () => {
  test('returns the last lines of a small log, without colour codes', async () => {
    const dir = tempDir()
    const log = join(dir, 'server.log')
    await writeFile(log, 'one\n\x1b[31mtwo\x1b[0m\nthree\n')
    expect(tailFile(log, 2)).toBe('two\nthree')
    expect(tailFile(join(dir, 'missing.log'))).toBe('(no log yet)')
  })

  test('reads only the end of a large log and drops the partial first line', async () => {
    const dir = tempDir()
    const log = join(dir, 'server.log')
    // ~6 MB of numbered lines; the tail must not depend on the size.
    const line = (n: number) => `line ${String(n).padStart(7, '0')} ${'x'.repeat(40)}\n`
    let body = ''
    for (let n = 0; n < 110_000; n++) body += line(n)
    await writeFile(log, body)
    await utimes(log, new Date(), new Date())
    const tail = tailFile(log, 3).split('\n')
    expect(tail).toEqual([109_997, 109_998, 109_999].map((n) => line(n).trimEnd()))
    // Every returned line is whole: none starts mid-line.
    for (const entry of tailFile(log, 2000).split('\n')) expect(entry).toMatch(/^line \d{7} x{40}$/)
    expect(tailFile(log, 2000).split('\n').length).toBeLessThan(1400)
  })
})

describe('destroySandboxes', () => {
  /** A stand-in for the sandbox API: lists what is left, destroys on request. */
  async function fakeApi(ids: string[], refuse: Record<string, number> = {}, listStatus = 200) {
    const alive = new Set(ids)
    const seen: string[] = []
    const server = createHttpServer((req, res) => {
      seen.push(`${req.method} ${req.url} ${req.headers.authorization}`)
      const destroy = /^\/api\/v1\/sandboxes\/([^/]+)\/destroy$/.exec(req.url ?? '')
      if (req.method === 'POST' && destroy) {
        const id = decodeURIComponent(destroy[1] as string)
        if (refuse[id]) return void res.writeHead(refuse[id] as number).end()
        alive.delete(id)
        return void res.writeHead(204).end()
      }
      if (listStatus !== 200) return void res.writeHead(listStatus).end()
      const query = new URL(req.url ?? '', 'http://localhost').searchParams
      const page = Number(query.get('page') ?? '1')
      const size = Number(query.get('page_size') ?? '20')
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end(JSON.stringify({ sandboxes: [...alive].slice((page - 1) * size, page * size).map((id) => ({ id })) }))
    })
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
    const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/api`
    return { url, alive, seen, close: () => new Promise((resolve) => server.close(resolve)) }
  }

  test('destroys every sandbox, across pages, with the API key', async () => {
    const ids = Array.from({ length: 230 }, (_, n) => `sbx_${n}`)
    const api = await fakeApi(ids)
    try {
      const result = await destroySandboxes(api.url, 'tk_test')
      expect(result.destroyed).toHaveLength(230)
      expect(result.failed).toEqual([])
      expect(api.alive.size).toBe(0)
      expect(api.seen.every((line) => line.endsWith('Bearer tk_test'))).toBe(true)
    } finally {
      await api.close()
    }
  })

  test('reports a sandbox the server refuses to destroy, once, and moves on', async () => {
    const api = await fakeApi(['a', 'busy', 'c'], { busy: 409 })
    try {
      const result = await destroySandboxes(api.url, 'tk_test')
      expect(result.destroyed.sort()).toEqual(['a', 'c'])
      expect(result.failed).toEqual(['busy: HTTP 409'])
      expect(api.seen.filter((line) => line.startsWith('POST /api/v1/sandboxes/busy/destroy'))).toHaveLength(1)
    } finally {
      await api.close()
    }
  })

  test('a full page of refused sandboxes does not hide the ones after it', async () => {
    const busy = Array.from({ length: 100 }, (_, n) => `busy_${n}`)
    const api = await fakeApi([...busy, 'x', 'y'], Object.fromEntries(busy.map((id) => [id, 409])))
    try {
      const result = await destroySandboxes(api.url, 'tk_test')
      expect(result.destroyed.sort()).toEqual(['x', 'y'])
      expect(result.failed).toHaveLength(100)
      // Every sandbox was tried exactly once.
      expect(api.seen.filter((line) => line.startsWith('POST '))).toHaveLength(102)
    } finally {
      await api.close()
    }
  })

  test('a server without the sandbox API has nothing to destroy; other failures are errors', async () => {
    const none = await fakeApi([], {}, 404)
    const broken = await fakeApi([], {}, 500)
    try {
      expect(await destroySandboxes(none.url, 'tk_test')).toEqual({ destroyed: [], failed: [] })
      await expect(destroySandboxes(broken.url, 'tk_test')).rejects.toThrow(/v1\/sandboxes returned HTTP 500/)
    } finally {
      await none.close()
      await broken.close()
    }
  })
})

describe('removeWorkloads', () => {
  const docker = (...args: string[]) => spawnSync('docker', args, { encoding: 'utf-8', timeout: 120_000 })
  const IMAGE = 'busybox:stable'

  test('removes the install network\'s containers, and only the volumes Temps created for them', async () => {
    if (docker('info', '--format', '{{.ServerVersion}}').status !== 0) {
      console.log('Docker not available, skipping')
      return
    }
    if (docker('image', 'inspect', IMAGE).status !== 0 && docker('pull', '--quiet', IMAGE).status !== 0) {
      console.log(`Could not pull ${IMAGE}, skipping`)
      return
    }
    const id = `temps-cli-test-${process.pid}-${Date.now()}`
    const n = (suffix: string) => `${id}-${suffix}`
    const stack = n('stack')
    const containers = ['service', 'app', 'compose', 'spoof', 'bystander'].map(n)
    const volumes = ['older', 'install', 'service-data', 'external', 'stack-data', 'other-stack-data', 'spoof-data', 'shared'].map(n)
    const cleanup = () => {
      docker('rm', '-f', '-v', ...containers)
      docker('volume', 'rm', ...volumes)
      docker('network', 'rm', n('net'), n('other-net'))
    }
    const ok = (...args: string[]) => {
      const result = docker(...args)
      expect([args.slice(0, 3).join(' '), result.status, result.stderr]).toEqual([args.slice(0, 3).join(' '), 0, ''])
    }
    const create = (name: string, network: string, labels: Record<string, string>, mounts: string[]) =>
      ok(
        'create', '--name', n(name), '--network', n(network),
        ...Object.entries(labels).flatMap(([key, value]) => ['--label', `${key}=${value}`]),
        ...mounts.flatMap((volume) => ['-v', `${n(volume)}:/mnt/${volume}`]),
        '-v', '/anonymous', IMAGE, 'true',
      )
    try {
      // A volume from before the install, then (a second later, the
      // resolution of Docker's CreatedAt) the install's own marker volume.
      ok('volume', 'create', n('older'))
      await new Promise((resolve) => setTimeout(resolve, 1100))
      for (const volume of ['install', 'service-data', 'external', 'spoof-data', 'shared']) ok('volume', 'create', n(volume))
      ok('volume', 'create', '--label', `com.docker.compose.project=${stack}`, n('stack-data'))
      ok('volume', 'create', '--label', 'com.docker.compose.project=someone-elses-stack', n('other-stack-data'))
      ok('network', 'create', n('net'))
      ok('network', 'create', n('other-net'))

      // A managed service: its own data volume, a volume older than the
      // install, and one a container outside the install also uses.
      create('service', 'net', { 'temps.service_type': 'redis', 'temps.service_name': 'cache' }, ['service-data', 'older', 'shared'])
      // An app pointed at a volume Temps did not create.
      create('app', 'net', { 'sh.temps.managed': 'true', 'sh.temps.project_id': '1' }, ['external'])
      // A Compose stack: the volume Compose made for it, and another stack's.
      create('compose', 'net', { 'sh.temps.managed': 'true', 'com.docker.compose.project': stack }, ['stack-data', 'other-stack-data'])
      // An app whose image ships a service label to pass for a managed service.
      create('spoof', 'net', { 'temps.service_type': 'redis', 'sh.temps.project_id': '2' }, ['spoof-data'])
      // Another Temps' container: same engine, different network.
      create('bystander', 'other-net', { 'temps.service_type': 'redis' }, ['shared'])

      const removed = await removeWorkloads(n('net'), n('install'))

      expect(removed.containers.sort()).toEqual(['app', 'compose', 'service', 'spoof'].map(n).sort())
      expect(removed.volumes.sort()).toEqual(['service-data', 'stack-data'].map(n).sort())
      const kept = removed.kept.join('\n')
      expect(removed.kept).toHaveLength(5)
      expect(kept).toMatch(new RegExp(`volume ${n('older')}: it existed before this install`))
      expect(kept).toMatch(new RegExp(`volume ${n('external')}: Temps did not create it \\(it was attached to ${n('app')}\\)`))
      expect(kept).toMatch(new RegExp(`volume ${n('other-stack-data')}: Temps did not create it`))
      expect(kept).toMatch(new RegExp(`volume ${n('spoof-data')}: Temps did not create it`))
      expect(kept).toMatch(new RegExp(`volume ${n('shared')}: .*in use`))

      const left = docker('ps', '-a', '--filter', `name=${id}`, '--format', '{{.Names}}').stdout.trim()
      expect(left).toBe(n('bystander'))
      const volumesLeft = docker('volume', 'ls', '--filter', `name=${id}`, '--format', '{{.Name}}').stdout.trim().split('\n').sort()
      expect(volumesLeft).toEqual(['external', 'install', 'older', 'other-stack-data', 'shared', 'spoof-data'].map(n).sort())
      expect(docker('network', 'inspect', n('net')).status).not.toBe(0)
      expect(docker('network', 'inspect', n('other-net')).status).toBe(0)

      // Nothing on the network: a no-op that reports nothing.
      expect(await removeWorkloads(n('net'), n('install'))).toEqual({ containers: [], volumes: [], kept: [] })
    } finally {
      cleanup()
    }
  }, 180_000)
})

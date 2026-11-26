// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  clearPendingLogin,
  getPendingLogin,
  pendingLoginExpired,
  pendingLoginsPath,
  savePendingLogin,
  type PendingLogin,
} from './pending-login.js'
import { getContext, upsertContext } from '../../config/contexts.js'
import {
  ContextTakenError,
  DEFAULT_RESUME_WAIT_SECS,
  DeviceLoginEndedError,
  LOGIN_STILL_PENDING_EXIT_CODE,
  LoginStillPendingError,
  formatPendingLoginInstructions,
  parseResumeWaitSecs,
  pollForDeviceApproval,
  resolveDeviceLoginMode,
  resumeDeviceLogin,
  startDeviceLogin,
} from './login.js'

const NOW = Date.parse('2026-11-25T14:00:00.000Z')

function pending(overrides: Partial<PendingLogin> = {}): PendingLogin {
  return {
    context: 'temps-example-com',
    apiBaseUrl: 'https://temps.example.com/api',
    deviceCode: 'device-code-1',
    userCode: 'ABCD-EFGH',
    verificationUrl: 'https://temps.example.com/cli-login/ABCD-EFGH',
    intervalSecs: 2,
    expiresAt: new Date(NOW + 15 * 60_000).toISOString(),
    ...overrides,
  }
}

let home: string
const originalHome = process.env.HOME

beforeEach(() => {
  home = mkdtempSync(join(tmpdir(), 'temps-pending-login-'))
  process.env.HOME = home
})

afterEach(() => {
  if (originalHome === undefined) delete process.env.HOME
  else process.env.HOME = originalHome
  rmSync(home, { recursive: true, force: true })
})

describe('pending login store', () => {
  test('saves a started sign-in in a file only the owner can read', async () => {
    await savePendingLogin(pending(), NOW)

    expect(pendingLoginsPath()).toBe(join(home, '.temps', '.pending-logins.json'))
    expect(statSync(pendingLoginsPath()).mode & 0o777).toBe(0o600)
    expect(await getPendingLogin('temps-example-com')).toEqual(pending())
  })

  test('returns null for a context that has no started sign-in', async () => {
    await savePendingLogin(pending(), NOW)
    expect(await getPendingLogin('other')).toBeNull()
  })

  test('starting again for the same context replaces the earlier device code', async () => {
    await savePendingLogin(pending(), NOW)
    await savePendingLogin(pending({ deviceCode: 'device-code-2' }), NOW)

    const stored = JSON.parse(readFileSync(pendingLoginsPath(), 'utf-8')) as PendingLogin[]
    expect(stored).toHaveLength(1)
    expect(stored[0]?.deviceCode).toBe('device-code-2')
  })

  test('drops expired sign-ins of other contexts when saving', async () => {
    await savePendingLogin(
      pending({ context: 'stale', expiresAt: new Date(NOW - 1).toISOString() }),
      NOW - 60_000,
    )
    await savePendingLogin(pending(), NOW)

    expect(await getPendingLogin('stale')).toBeNull()
    expect(await getPendingLogin('temps-example-com')).not.toBeNull()
  })

  test('clearing by device code leaves a newer sign-in for the same context alone', async () => {
    // A --resume that finishes after a new --no-wait replaced its entry.
    await savePendingLogin(pending({ deviceCode: 'device-code-newer' }), NOW)
    await clearPendingLogin('temps-example-com', 'device-code-1')
    expect((await getPendingLogin('temps-example-com'))?.deviceCode).toBe('device-code-newer')

    await clearPendingLogin('temps-example-com', 'device-code-newer')
    expect(await getPendingLogin('temps-example-com')).toBeNull()
  })

  test('clearing the last sign-in removes the file', async () => {
    await savePendingLogin(pending(), NOW)
    await clearPendingLogin('temps-example-com')

    expect(existsSync(pendingLoginsPath())).toBe(false)
    expect(await getPendingLogin('temps-example-com')).toBeNull()
  })

  test('a malformed file means nothing is pending', async () => {
    await savePendingLogin(pending(), NOW)
    await Bun.write(pendingLoginsPath(), '{not json')
    expect(await getPendingLogin('temps-example-com')).toBeNull()
  })

  test('expiry is judged against the given time, and a bad date counts as expired', () => {
    expect(pendingLoginExpired(pending(), NOW)).toBe(false)
    expect(pendingLoginExpired(pending(), NOW + 15 * 60_000)).toBe(true)
    expect(pendingLoginExpired(pending({ expiresAt: 'soon' }), NOW)).toBe(true)
  })
})

describe('resolveDeviceLoginMode', () => {
  test('waits by default', () => {
    expect(resolveDeviceLoginMode({})).toBe('wait')
    expect(resolveDeviceLoginMode({ wait: true })).toBe('wait')
  })

  test('--no-wait starts and --resume finishes', () => {
    expect(resolveDeviceLoginMode({ wait: false })).toBe('start')
    expect(resolveDeviceLoginMode({ resume: true })).toBe('resume')
  })

  test('refuses --no-wait together with --resume', () => {
    expect(() => resolveDeviceLoginMode({ wait: false, resume: true })).toThrow(
      /cannot be combined/,
    )
  })

  test('refuses either flag with --api-key, which needs no approval', () => {
    expect(() => resolveDeviceLoginMode({ wait: false, apiKey: 'tk_example' })).toThrow(
      /--no-wait applies to the browser sign-in only/,
    )
    expect(() => resolveDeviceLoginMode({ resume: true, apiKey: 'tk_example' })).toThrow(
      /--resume applies to the browser sign-in only/,
    )
  })
})

describe('parseResumeWaitSecs', () => {
  test('defaults under a two-minute tool timeout', () => {
    expect(parseResumeWaitSecs(undefined)).toBe(DEFAULT_RESUME_WAIT_SECS)
    expect(DEFAULT_RESUME_WAIT_SECS).toBeLessThan(120)
  })

  test('accepts a positive whole number', () => {
    expect(parseResumeWaitSecs('300')).toBe(300)
  })

  test.each(['0', '-5', '1.5', 'soon', ''])('rejects %p', (value) => {
    expect(() => parseResumeWaitSecs(value)).toThrow(/--wait-secs must be a whole number/)
  })
})

describe('formatPendingLoginInstructions', () => {
  test('puts the link alone on a line and names the command that finishes', () => {
    const lines = formatPendingLoginInstructions(pending(), NOW)

    expect(lines).toContain('  https://temps.example.com/cli-login/ABCD-EFGH')
    expect(lines).toContain('Code: ABCD-EFGH (valid for 15 minutes)')
    expect(lines).toContain(
      '  temps login https://temps.example.com --context temps-example-com --resume',
    )
    expect(lines.join('\n')).toContain('Nothing is signed in yet')
  })

  test('carries no colour codes or box drawing an agent would have to strip', () => {
    const text = formatPendingLoginInstructions(pending(), NOW).join('\n')
    // eslint-disable-next-line no-control-regex
    expect(text).not.toMatch(/\u001b\[|[╭╮╰╯│─]/)
  })
})

describe('pollForDeviceApproval with a bounded wait', () => {
  test('throws the caller-supplied error when the wait runs out while pending', async () => {
    let clock = NOW
    const run = pollForDeviceApproval({
      pollUrl: 'https://temps.example.com/api/auth/cli/device/poll',
      intervalSecs: 2,
      expiresInSecs: 5,
      timeoutError: () => new LoginStillPendingError(pending(), 5),
      sleep: async (ms) => {
        clock += ms
      },
      now: () => clock,
      poll: async () => ({
        res: new Response('{}', { status: 200 }),
        rawBody: '{"status":"authorization_pending"}',
        json: { status: 'authorization_pending' },
      }),
    })

    const error = await run.catch((e: unknown) => e)
    expect(error).toBeInstanceOf(LoginStillPendingError)
    expect((error as LoginStillPendingError).exitCode).toBe(LOGIN_STILL_PENDING_EXIT_CODE)
    expect((error as Error).message).toContain('https://temps.example.com/cli-login/ABCD-EFGH')
    expect((error as Error).message).toContain('Nothing is signed in yet')
  })
})

/** A device-authorization server whose poll answer the test controls. */
function fakeDeviceServer(pollStatus: () => string | Response) {
  const requests: string[] = []
  const server = Bun.serve({
    port: 0,
    hostname: '127.0.0.1',
    async fetch(req) {
      const url = new URL(req.url)
      requests.push(url.pathname)
      if (url.pathname === '/api/auth/cli/device/start') {
        return Response.json({
          device_code: 'device-code-live',
          user_code: 'WXYZ-1234',
          verification_uri: '/cli-login',
          verification_uri_complete: '/cli-login/WXYZ-1234',
          expires_in: 900,
          interval: 1,
        })
      }
      if (url.pathname === '/api/auth/cli/device/poll') {
        const answer = pollStatus()
        return typeof answer === 'string' ? Response.json({ status: answer }) : answer
      }
      return new Response('not found', { status: 404 })
    },
  })
  return { server, requests, origin: `http://127.0.0.1:${server.port}` }
}

describe('split browser sign-in against a device-authorization server', () => {
  test('--no-wait prints the link, remembers the device code and does not poll', async () => {
    const fake = fakeDeviceServer(() => 'authorization_pending')
    const printed: string[] = []
    const originalLog = console.log
    console.log = (...args: unknown[]) => {
      printed.push(args.join(' '))
    }
    try {
      await startDeviceLogin({ url: fake.origin, context: 'local' })
    } finally {
      console.log = originalLog
      await fake.server.stop(true)
    }

    expect(printed).toContain(`  ${fake.origin}/cli-login/WXYZ-1234`)
    expect(fake.requests).toEqual(['/api/auth/cli/device/start'])
    const stored = await getPendingLogin('local')
    expect(stored?.deviceCode).toBe('device-code-live')
    expect(stored?.apiBaseUrl).toBe(`${fake.origin}/api`)
    expect(stored?.verificationUrl).toBe(`${fake.origin}/cli-login/WXYZ-1234`)
  })

  test('--resume reports "not yet" with exit code 2 and keeps the sign-in for a rerun', async () => {
    const fake = fakeDeviceServer(() => 'authorization_pending')
    await savePendingLogin(
      pending({
        context: 'local',
        apiBaseUrl: `${fake.origin}/api`,
        intervalSecs: 1,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'local', waitSecs: 1 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect(error).toBeInstanceOf(LoginStillPendingError)
    expect((error as LoginStillPendingError).exitCode).toBe(2)
    expect(await getPendingLogin('local')).not.toBeNull()
  })

  test('--resume forgets a sign-in the person denied', async () => {
    const fake = fakeDeviceServer(() => 'access_denied')
    await savePendingLogin(
      pending({
        context: 'local',
        apiBaseUrl: `${fake.origin}/api`,
        intervalSecs: 1,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'local', waitSecs: 5 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect((error as Error).message).toBe('Authorization denied in the browser.')
    expect(await getPendingLogin('local')).toBeNull()
  })

  test('--resume without a started sign-in says how to start one', async () => {
    await expect(
      resumeDeviceLogin({ url: 'https://temps.example.com', context: 'temps-example-com' }),
    ).rejects.toThrow(
      'No sign-in is waiting for context "temps-example-com". Start one with: temps login https://temps.example.com --context temps-example-com --no-wait',
    )
  })

  test('--resume refuses a sign-in started against another server', async () => {
    await savePendingLogin(
      pending({ expiresAt: new Date(Date.now() + 15 * 60_000).toISOString() }),
    )
    await expect(
      resumeDeviceLogin({ url: 'https://other.example.com', context: 'temps-example-com' }),
    ).rejects.toThrow(/was started against https:\/\/temps\.example\.com\/api, not https:\/\/other\.example\.com\/api/)
    expect(await getPendingLogin('temps-example-com')).not.toBeNull()
  })

  test('--resume clears an expired sign-in and asks for a new one', async () => {
    await savePendingLogin(pending({ expiresAt: new Date(Date.now() - 1000).toISOString() }))
    await expect(
      resumeDeviceLogin({ url: 'https://temps.example.com', context: 'temps-example-com' }),
    ).rejects.toThrow(/expired before it was approved/)
    expect(await getPendingLogin('temps-example-com')).toBeNull()
  })

  test('--resume stops at its deadline even when the server never finishes answering', async () => {
    // Headers arrive, the body never ends: only the deadline can end the wait.
    const fake = fakeDeviceServer(
      () => new Response(new ReadableStream({ start() {} }), { status: 200 }),
    )
    await savePendingLogin(
      pending({
        context: 'local',
        apiBaseUrl: `${fake.origin}/api`,
        intervalSecs: 1,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    const started = Date.now()
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'local', waitSecs: 2 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect(error).toBeInstanceOf(LoginStillPendingError)
    expect(Date.now() - started).toBeLessThan(5_000)
    expect(await getPendingLogin('local')).not.toBeNull()
  })

  test('--resume keeps the sign-in when the server fails, and says to run it again', async () => {
    const fake = fakeDeviceServer(
      () => Response.json({ title: 'Service Unavailable', detail: 'restarting' }, { status: 503 }),
    )
    await savePendingLogin(
      pending({
        context: 'local',
        apiBaseUrl: `${fake.origin}/api`,
        intervalSecs: 1,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'local', waitSecs: 5 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect((error as Error).message).toBe(
      'restarting The sign-in is still waiting: run the same command again to keep waiting for the same approval.',
    )
    expect(await getPendingLogin('local')).not.toBeNull()
  })

  test('--resume forgets a device code the server refuses', async () => {
    const fake = fakeDeviceServer(
      () => Response.json({ title: 'Bad Request', detail: 'unknown device code' }, { status: 400 }),
    )
    await savePendingLogin(
      pending({
        context: 'local',
        apiBaseUrl: `${fake.origin}/api`,
        intervalSecs: 1,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'local', waitSecs: 5 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect(error).toBeInstanceOf(DeviceLoginEndedError)
    expect(await getPendingLogin('local')).toBeNull()
  })
})

describe('a context name that belongs to another server', () => {
  const theirs = {
    name: 'temps-example-com',
    url: 'https://temps-example.com/api',
    apiKey: 'tk_someone_elses',
    email: 'person@example.com',
  }

  test('--no-wait refuses before asking the server for anything, and changes nothing', async () => {
    await upsertContext(theirs)
    const fake = fakeDeviceServer(() => 'authorization_pending')
    let error: unknown
    try {
      await startDeviceLogin({ url: 'https://temps.example.com', context: 'temps-example-com' })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect(error).toBeInstanceOf(ContextTakenError)
    expect((error as Error).message).toBe(
      'Context "temps-example-com" already points to https://temps-example.com/api, not https://temps.example.com/api. Nothing was changed. ' +
        'Sign in under another name with --context <name>, or run `temps context remove temps-example-com` first if that context is no longer used.',
    )
    expect(fake.requests).toEqual([])
    expect(await getPendingLogin('temps-example-com')).toBeNull()
    expect(await getContext('temps-example-com')).toMatchObject(theirs)
  })

  test('--resume refuses too, without polling, and keeps the started sign-in', async () => {
    const fake = fakeDeviceServer(() => 'approved')
    await savePendingLogin(
      pending({
        apiBaseUrl: `${fake.origin}/api`,
        expiresAt: new Date(Date.now() + 15 * 60_000).toISOString(),
      }),
    )
    await upsertContext(theirs)
    let error: unknown
    try {
      await resumeDeviceLogin({ url: fake.origin, context: 'temps-example-com', waitSecs: 5 })
    } catch (e) {
      error = e
    } finally {
      await fake.server.stop(true)
    }

    expect(error).toBeInstanceOf(ContextTakenError)
    expect(fake.requests).toEqual([])
    expect(await getPendingLogin('temps-example-com')).not.toBeNull()
    expect(await getContext('temps-example-com')).toMatchObject(theirs)
  })

  test('the same server written differently is not another server', async () => {
    await upsertContext({ ...theirs, url: 'https://TEMPS.example.com/api/' })
    const fake = fakeDeviceServer(() => 'authorization_pending')
    const originalLog = console.log
    console.log = () => {}
    try {
      await startDeviceLogin({ url: fake.origin, context: 'local' })
      await upsertContext({ ...theirs, name: 'local', url: `${fake.origin}/` })
      await startDeviceLogin({ url: `${fake.origin}/api`, context: 'local' })
    } finally {
      console.log = originalLog
      await fake.server.stop(true)
    }
    expect((await getPendingLogin('local'))?.apiBaseUrl).toBe(`${fake.origin}/api`)
  })
})

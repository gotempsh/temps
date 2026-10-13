// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  LoginFlowError,
  runProviderLogin,
  type LoginIo,
  type LoginPhase,
  type LoginSession,
  type ProviderLoginApi,
} from './provider-login'

function session(
  phase: LoginPhase,
  extra: Partial<LoginSession> = {}
): LoginSession {
  return {
    session_id: 's1',
    provider_id: 'claude_cli',
    phase,
    detail: '',
    requires_code: true,
    elapsed_seconds: 1,
    timeout_seconds: 600,
    ...extra,
  }
}

function fakeApi(script: LoginSession[]) {
  const calls: string[] = []
  const submitted: string[] = []
  let index = 0
  const next = (): LoginSession =>
    script[Math.min(index++, script.length - 1)] as LoginSession
  const api: ProviderLoginApi = {
    async start() {
      calls.push('start')
      return next()
    },
    async status() {
      calls.push('status')
      return next()
    },
    async submitCode(_id, code) {
      calls.push('submit')
      submitted.push(code)
      return next()
    },
    async cancel() {
      calls.push('cancel')
      return session('cancelled')
    },
  }
  return { api, calls, submitted }
}

function fakeIo(code = 'good-code') {
  const lines: string[] = []
  const prompts: string[] = []
  const io: LoginIo = {
    log: (line) => lines.push(line),
    promptCode: async (message) => {
      prompts.push(message)
      return code
    },
    sleep: async () => undefined,
  }
  return { io, lines, prompts }
}

const LINK = 'https://claude.ai/oauth/authorize?code=true&state=abc'

describe('runProviderLogin', () => {
  test('prints the link, prompts for the code and reports success', async () => {
    const { api, calls, submitted } = fakeApi([
      session('starting'),
      session('waiting', { verification_url: LINK }),
      session('verifying'),
      session('succeeded', { detail: 'Signed in (max plan).' }),
    ])
    const { io, lines, prompts } = fakeIo()

    const result = await runProviderLogin(api, io, 'claude_cli', {
      pollIntervalMs: 0,
    })

    expect(result.phase).toBe('succeeded')
    expect(submitted).toEqual(['good-code'])
    expect(prompts).toHaveLength(1)
    expect(lines.some((line) => line.includes(LINK))).toBe(true)
    // The pasted code is never echoed back.
    expect(lines.join('\n')).not.toContain('good-code')
    expect(calls[0]).toBe('start')
  })

  test('device-code flows show the user code and never prompt', async () => {
    const { api, submitted } = fakeApi([
      session('waiting', {
        provider_id: 'codex_cli',
        verification_url: 'https://auth.openai.com/codex/device',
        user_code: 'ABCD-1234',
        requires_code: false,
      }),
      session('waiting', {
        provider_id: 'codex_cli',
        verification_url: 'https://auth.openai.com/codex/device',
        user_code: 'ABCD-1234',
        requires_code: false,
      }),
      session('succeeded', { provider_id: 'codex_cli' }),
    ])
    const { io, lines, prompts } = fakeIo()

    await runProviderLogin(api, io, 'codex_cli', { pollIntervalMs: 0 })

    expect(prompts).toHaveLength(0)
    expect(submitted).toHaveLength(0)
    expect(lines.some((line) => line.includes('ABCD-1234'))).toBe(true)
  })

  test('a rejected code surfaces the server detail', async () => {
    const { api } = fakeApi([
      session('waiting', { verification_url: LINK }),
      session('verifying'),
      session('failed', { detail: 'the CLI rejected the code' }),
    ])
    const { io } = fakeIo()

    await expect(
      runProviderLogin(api, io, 'claude_cli', { pollIntervalMs: 0 })
    ).rejects.toMatchObject({
      name: 'LoginFlowError',
      phase: 'failed',
      message: 'the CLI rejected the code',
    })
  })

  test('an empty code cancels the sign-in instead of submitting it', async () => {
    const { api, calls, submitted } = fakeApi([
      session('waiting', { verification_url: LINK }),
    ])
    const { io } = fakeIo('   ')

    await expect(
      runProviderLogin(api, io, 'claude_cli', { pollIntervalMs: 0 })
    ).rejects.toBeInstanceOf(LoginFlowError)
    expect(submitted).toHaveLength(0)
    expect(calls).toContain('cancel')
  })

  test('exposes a cancel hook for Ctrl-C', async () => {
    const { api, calls } = fakeApi([session('succeeded')])
    const { io } = fakeIo()
    let cancel: (() => Promise<void>) | undefined

    await runProviderLogin(api, io, 'claude_cli', {
      pollIntervalMs: 0,
      onCancelable: (fn) => {
        cancel = fn
      },
    })
    await cancel?.()

    expect(calls).toContain('cancel')
  })
})

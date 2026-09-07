// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, describe, expect, test } from 'bun:test'
import TempsChatProvider, {
  normalizeApiUrl,
  parseSse,
  redactSensitiveText,
} from './temps-chat-provider.ts'

const ORIGINAL_ENV = { ...process.env }

function response(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(typeof body === 'string' ? body : JSON.stringify(body), {
    status,
    headers: headers ?? { 'Content-Type': 'application/json' },
  })
}

describe('Temps Promptfoo provider', () => {
  beforeEach(() => {
    process.env.TEMPS_URL = 'http://localhost:3021'
    process.env.TEMPS_API_KEY = 'tk_test_secret_123456789'
    process.env.TEMPS_AI_EVAL_PROJECT_ID = '7'
    delete process.env.TEMPS_AI_EVAL_KEEP_CONVERSATIONS
    delete process.env.TEMPS_AI_EVAL_ALLOW_REMOTE
  })

  afterEach(() => {
    process.env = { ...ORIGINAL_ENV }
  })

  test('parses CRLF SSE frames and multi-line data', () => {
    expect(parseSse('event: tool_call\r\ndata: {"name":"temps",\r\ndata: "arguments":"{}"}\r\n\r\nevent: turn_complete\r\ndata:\r\n\r\n')).toEqual([
      { event: 'tool_call', data: '{"name":"temps",\n"arguments":"{}"}' },
      { event: 'turn_complete', data: '' },
    ])
  })

  test('redacts Temps, OpenAI, Anthropic, bearer, and JSON token values', () => {
    const key = 'tk_test_secret_123456789'
    const input = `${key} sk-abcdefghijk sk-ant-abcdefghijk Bearer abc.def {"refresh_token":"oauth-secret","password":"plain-secret"}`
    const output = redactSensitiveText(input, key)
    expect(output).not.toContain(key)
    expect(output).not.toContain('abcdefghijk')
    expect(output).not.toContain('abc.def')
    expect(output).not.toContain('oauth-secret')
    expect(output).not.toContain('plain-secret')
  })

  test('refuses remote HTTP targets even when remote execution is opted in', () => {
    process.env.TEMPS_AI_EVAL_ALLOW_REMOTE = '1'
    expect(() => normalizeApiUrl('http://temps.example.com')).toThrow('must use HTTPS')
  })

  test('runs through Temps, records tool metadata, and archives the temporary conversation', async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = []
    const fetchMock = (async (input: URL | RequestInfo, init?: RequestInit) => {
      const url = String(input)
      requests.push({ url, init })
      if (url.endsWith('/ai/conversations')) {
        return response({ public_id: 'conv-1', ai_provider: 'claude_cli' })
      }
      if (url.endsWith('/messages')) {
        return response(
          'event: tool_call\ndata: {"id":"1","name":"temps","arguments":"{\\"command\\":\\"projects get_projects --page 1\\"}"}\n\ndata: I found one project.\n\nevent: turn_complete\ndata:\n\n',
          200,
          { 'Content-Type': 'text/event-stream' },
        )
      }
      if (url.endsWith('/pending-actions')) return response([])
      if (url.endsWith('/archive')) return response('', 204)
      return response({ messages: [{ role: 'assistant', content: 'I found one project.' }] })
    }) as typeof fetch

    const provider = new TempsChatProvider(
      { id: 'test-claude', config: { aiProvider: 'claude_cli', permissionMode: 'default' } },
      fetchMock,
    )
    const result = await provider.callApi('List projects')

    expect(result.error).toBeUndefined()
    expect(result.output).toBe('I found one project.')
    expect(result.metadata?.operations).toEqual(['get_projects'])
    expect(result.metadata?.toolCalls).toEqual([
      {
        name: 'temps',
        operation: 'get_projects',
        command: 'projects get_projects --page 1',
        sequence: 0,
      },
    ])
    expect(result.metadata?.pendingActions).toEqual([])
    expect(requests.at(-1)?.url).toEndWith('/archive')
    expect(requests[0]?.init?.body).toContain('promptfoo-claude_cli-')
    expect(JSON.stringify(result)).not.toContain('tk_test_secret_123456789')
  })

  test('records redacted server-side proposal state without confirming it', async () => {
    const fetchMock = (async (input: URL | RequestInfo) => {
      const url = String(input)
      if (url.endsWith('/ai/conversations')) {
        return response({ public_id: 'conv-proposal', ai_provider: 'codex_cli' })
      }
      if (url.endsWith('/messages')) {
        return response(
          'event: tool_call\ndata: {"name":"temps","arguments":"{\\"command\\":\\"projects get_project --project_id 7\\"}"}\n\nevent: tool_call\ndata: {"name":"temps_write","arguments":"{\\"command\\":\\"environments update_environment_settings --project_id 7 --automatic_deploy true\\"}"}\n\nevent: turn_complete\ndata:\n\n',
          200,
          { 'Content-Type': 'text/event-stream' },
        )
      }
      if (url.endsWith('/pending-actions')) {
        return response([
          {
            operation_id: 'update_environment_settings',
            method: 'PATCH',
            status: 'proposed',
            required_permission: 'projects:write',
            params: { project_id: 7, token: 'tk_private_value' },
          },
        ])
      }
      if (url.endsWith('/archive')) return response('', 204)
      return response({ messages: [{ role: 'assistant', content: 'The environment override is off.' }] })
    }) as typeof fetch

    const provider = new TempsChatProvider(
      { config: { aiProvider: 'codex_cli', permissionMode: 'auto' } },
      fetchMock,
    )
    const result = await provider.callApi('Diagnose automatic deployments')

    expect(result.error).toBeUndefined()
    expect(result.metadata?.operations).toEqual([
      'get_project',
      'update_environment_settings',
    ])
    expect(result.metadata?.pendingActions).toEqual([
      {
        operationId: 'update_environment_settings',
        method: 'PATCH',
        status: 'proposed',
        requiredPermission: 'projects:write',
        params: { project_id: 7, token: '[REDACTED]' },
      },
    ])
    expect(JSON.stringify(result)).not.toContain('tk_private_value')
  })

  test('sanitizes API error bodies and still archives an already-created conversation', async () => {
    const urls: string[] = []
    const fetchMock = (async (input: URL | RequestInfo) => {
      const url = String(input)
      urls.push(url)
      if (url.endsWith('/ai/conversations')) {
        return response({ public_id: 'conv-2', ai_provider: 'codex_cli' })
      }
      if (url.endsWith('/messages')) {
        return response('Bearer super-secret tk_test_secret_123456789', 500)
      }
      if (url.endsWith('/archive')) return response('', 204)
      throw new Error(`unexpected URL: ${url}`)
    }) as typeof fetch

    const provider = new TempsChatProvider(
      { id: 'test-codex', config: { aiProvider: 'codex_cli', permissionMode: 'auto' } },
      fetchMock,
    )
    const result = await provider.callApi('List projects')

    expect(result.error).toContain('HTTP 500')
    expect(result.error).not.toContain('super-secret')
    expect(result.error).not.toContain('tk_test_secret_123456789')
    expect(urls.at(-1)).toEndWith('/archive')
  })
})

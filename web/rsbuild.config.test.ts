// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'

import {
  deriveConsoleTarget,
  isApiHttpRequest,
  isApiWebSocket,
  isWebSocketUpgrade,
} from './rsbuild.config'

describe('deriveConsoleTarget', () => {
  it('keeps a non-zero dev slot on its matching Console listener', () => {
    expect(deriveConsoleTarget('http://localhost:8220')).toBe(
      'http://localhost:8221'
    )
  })

  it('preserves the slot-zero default', () => {
    expect(deriveConsoleTarget('http://localhost:8080')).toBe(
      'http://localhost:8081'
    )
  })
})

describe('API proxy routing', () => {
  const upgrade = { headers: { upgrade: 'websocket', connection: 'Upgrade' } }
  const plain = { headers: {} }

  it('sends every API WebSocket to the Console listener, not only chat', () => {
    for (const path of [
      '/api/projects/1/deployments/2/jobs/build/logs/tail',
      '/api/projects/1/environments/2/containers/abc/logs',
      '/api/projects/1/ai/conversations/xyz/stream',
    ]) {
      expect(isApiWebSocket(path, upgrade)).toBe(true)
      expect(isApiHttpRequest(path, upgrade)).toBe(false)
    }
  })

  it('keeps ordinary API requests on the API listener', () => {
    const path = '/api/projects/1/deployments/2/jobs/build/logs'
    expect(isApiHttpRequest(path, plain)).toBe(true)
    expect(isApiWebSocket(path, plain)).toBe(false)
  })

  it('matches the Upgrade header case-insensitively', () => {
    expect(isWebSocketUpgrade({ headers: { upgrade: 'WebSocket' } })).toBe(true)
    expect(isWebSocketUpgrade({ headers: { upgrade: ['websocket'] } })).toBe(
      true
    )
    expect(isWebSocketUpgrade({ headers: { upgrade: 'h2c' } })).toBe(false)
  })

  it('leaves non-API sockets such as dev-server HMR alone', () => {
    expect(isApiWebSocket('/rsbuild-hmr', upgrade)).toBe(false)
    expect(isApiHttpRequest('/apiary', plain)).toBe(false)
  })
})

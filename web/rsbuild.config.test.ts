// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { readFileSync } from 'node:fs'

import {
  consoleSocketProxy,
  deriveConsoleTarget,
  htmlTemplatePath,
  isApiHttpRequest,
  isApiWebSocket,
  isLocalTarget,
  isWebSocketUpgrade,
  rewriteDevServerOrigin,
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

describe('isLocalTarget', () => {
  it('keeps the browser Host only for a listener on this machine', () => {
    expect(isLocalTarget('http://localhost:8081')).toBe(true)
    expect(isLocalTarget('http://127.0.0.1:8081')).toBe(true)
    expect(isLocalTarget('http://[::1]:8081')).toBe(true)
  })

  it('treats anything else as a remote instance that routes by Host', () => {
    expect(isLocalTarget('https://console.example.test')).toBe(false)
    expect(isLocalTarget('https://localhost.example.test')).toBe(false)
    expect(isLocalTarget('https://127.example.test')).toBe(false)
  })
})

describe('rewriteDevServerOrigin', () => {
  const dev = 'http://127.0.0.1:5173'
  const target = 'http://127.0.0.1:8080'

  it('maps the dev server Origin and Referer onto the API target', () => {
    expect(rewriteDevServerOrigin(dev, dev, target)).toBe(target)
    expect(rewriteDevServerOrigin(`${dev}/email/domains/4`, dev, target)).toBe(
      `${target}/email/domains/4`
    )
  })

  it('leaves foreign and look-alike origins untouched', () => {
    expect(
      rewriteDevServerOrigin('https://attacker.example', dev, target)
    ).toBeUndefined()
    expect(
      rewriteDevServerOrigin(`${dev}.attacker.example`, dev, target)
    ).toBeUndefined()
    expect(rewriteDevServerOrigin(undefined, dev, target)).toBeUndefined()
  })
})

describe('HTML shell', () => {
  const template = readFileSync(htmlTemplatePath, 'utf8')

  it('declares the document language on <html>', () => {
    expect(template).toMatch(/<html[^>]*\slang="en"/)
  })

  it('keeps the mount point Rsbuild renders the app into', () => {
    expect(template).toContain('<div id="<%= mountId %>"></div>')
  })
})

describe('consoleSocketProxy', () => {
  const socketHeaders = (target: string, origin?: string) => {
    const set: Record<string, string> = {}
    consoleSocketProxy(target).on.proxyReqWs(
      { setHeader: (name, value) => (set[name] = value) },
      { headers: { origin, host: 'localhost:3000' } }
    )
    return set
  }

  it('sends a remote instance its own Host and, to match, its own Origin', () => {
    const target = 'https://console.example.test'
    expect(consoleSocketProxy(target).changeOrigin).toBe(true)
    expect(socketHeaders(target, 'http://localhost:3000')).toEqual({
      origin: 'https://console.example.test',
    })
  })

  it('leaves Host and Origin as the browser sent them for a local listener', () => {
    const target = 'http://localhost:8081'
    expect(consoleSocketProxy(target).changeOrigin).toBe(false)
    expect(socketHeaders(target, 'http://localhost:3000')).toEqual({})
  })

  it('never turns a foreign Origin into the target, so the guard still refuses it', () => {
    const target = 'https://console.example.test'
    expect(socketHeaders(target, 'https://other.example.test')).toEqual({})
    expect(socketHeaders(target, undefined)).toEqual({})
  })
})

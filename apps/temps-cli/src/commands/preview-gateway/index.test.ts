// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildPreviewGatewayPatch,
  buildUpgradeImage,
  parseHostPort,
  parseTail,
  registerPreviewGatewayCommands,
} from './index.js'
import { runWithApi } from '../../lib/mock-api.js'

const settings = {
  enabled: true,
  image: 'ghcr.io/example/preview-gateway@sha256:aaaa',
  default_image: 'ghcr.io/example/preview-gateway@sha256:aaaa',
  host_port: 18080,
  default_host_port: 18080,
  auto_upgrade: true,
  container_name: 'temps-preview-gateway',
  default_container_name: 'temps-preview-gateway',
}

const status = {
  present: true,
  running: true,
  health: 'running',
  container_name: 'temps-preview-gateway',
  expected_image: 'ghcr.io/example/preview-gateway@sha256:aaaa',
  image: 'ghcr.io/example/preview-gateway@sha256:aaaa',
  drift: false,
  auto_upgrade: true,
  host_port: 18080,
  setup_error: null,
}

describe('parseTail and parseHostPort', () => {
  test('accept whole numbers in range', () => {
    expect(parseTail(undefined)).toBeUndefined()
    expect(parseTail('1')).toBe(1)
    expect(parseTail('2000')).toBe(2000)
    expect(parseHostPort('8080')).toBe(8080)
    expect(parseHostPort('65535')).toBe(65535)
  })

  test('reject zero, out of range, fractions and words, naming the flag', () => {
    for (const raw of ['0', '2001', '1.5', 'ten', '', '-3']) {
      expect(() => parseTail(raw)).toThrow(/--tail must be/)
    }
    for (const raw of ['0', '65536', '80.5', 'http', '']) {
      expect(() => parseHostPort(raw)).toThrow(/--host-port must be/)
    }
  })
})

describe('buildPreviewGatewayPatch', () => {
  test('sends only the flags given', () => {
    expect(buildPreviewGatewayPatch({ hostPort: '9090' }, { enabled: false, host_port: 8080 })).toEqual({ body: { host_port: 9090 }, disruptions: [] })
    expect(buildPreviewGatewayPatch({ autoUpgrade: false })).toEqual({ body: { auto_upgrade: false }, disruptions: [] })
    expect(buildPreviewGatewayPatch({ enable: true, image: ' img@sha256:b ' })).toEqual({
      body: { enabled: true, image: 'img@sha256:b' },
      disruptions: [],
    })
  })

  test('resets the image and container name with an empty value', () => {
    expect(buildPreviewGatewayPatch({ resetImage: true }).body).toEqual({ image: '' })
    expect(buildPreviewGatewayPatch({ resetContainerName: true }).body).toEqual({ container_name: '' })
  })

  test('flags disabling and renaming as interrupting previews', () => {
    expect(buildPreviewGatewayPatch({ disable: true }).disruptions).toHaveLength(1)
    // A port change on a running gateway recreates it; the same port, or a stopped gateway, does not.
    expect(buildPreviewGatewayPatch({ hostPort: '9090' }, { enabled: true, host_port: 8080 }).disruptions[0]).toMatch(/host port/)
    expect(buildPreviewGatewayPatch({ hostPort: '8080' }, { enabled: true, host_port: 8080 }).disruptions).toEqual([])
    expect(buildPreviewGatewayPatch({ hostPort: '9090' }).disruptions).toHaveLength(1)
    expect(buildPreviewGatewayPatch({ hostPort: '9090', disable: true }, { enabled: true, host_port: 8080 }).disruptions).toHaveLength(1)
    expect(buildPreviewGatewayPatch({ containerName: 'temps-b-gateway' }).disruptions[0]).toMatch(/Renaming/)
    expect(buildPreviewGatewayPatch({ disable: true, containerName: 'x' }).disruptions).toHaveLength(2)
  })

  test('refuses contradictory, empty and missing flags', () => {
    expect(() => buildPreviewGatewayPatch({})).toThrow(/Nothing to change/)
    expect(() => buildPreviewGatewayPatch({ enable: true, disable: true })).toThrow(/not both/)
    expect(() => buildPreviewGatewayPatch({ image: 'a', resetImage: true })).toThrow(/not both/)
    expect(() => buildPreviewGatewayPatch({ containerName: 'a', resetContainerName: true })).toThrow(/not both/)
    expect(() => buildPreviewGatewayPatch({ image: '  ' })).toThrow(/--reset-image/)
    expect(() => buildPreviewGatewayPatch({ containerName: '' })).toThrow(/--reset-container-name/)
  })
})

describe('buildUpgradeImage', () => {
  test('takes an explicit image, or the empty default', () => {
    expect(buildUpgradeImage({ image: 'img@sha256:c' })).toBe('img@sha256:c')
    expect(buildUpgradeImage({ default: true })).toBe('')
  })

  test('requires exactly one of --image and --default', () => {
    expect(() => buildUpgradeImage({})).toThrow(/--image <ref>/)
    expect(() => buildUpgradeImage({ image: 'a', default: true })).toThrow(/not both/)
  })
})

describe('temps preview-gateway', () => {
  test('status --json prints the full status', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'status', '--json'], {
      'GET /preview-gateway/status': status,
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout)).toEqual(status)
  })

  test('status shows the setup error and when Temps retries', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'status'], {
      'GET /preview-gateway/status': {
        ...status,
        present: false,
        running: false,
        health: 'missing',
        setup_error: 'pull image: manifest unknown',
        next_setup_attempt_at: '2026-01-01T00:00:00Z',
      },
    })
    expect(run.error).toBeUndefined()
    expect(run.stdout).toContain('pull image: manifest unknown')
    expect(run.stdout).toContain('2026-01-01T00:00:00Z')
  })

  test('settings --json prints the settings with their defaults', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'settings', '--json'], {
      'GET /preview-gateway/settings': settings,
    })
    expect(run.error).toBeUndefined()
    expect(JSON.parse(run.stdout)).toEqual(settings)
  })

  test('configure sends a partial PATCH and prints the saved settings', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'configure', '--host-port', '9090', '--no-auto-upgrade', '--yes', '--json'],
      {
        'GET /preview-gateway/settings': settings,
        'PATCH /preview-gateway/settings': { ...settings, host_port: 9090, auto_upgrade: false },
      },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([
      { request: 'PATCH /preview-gateway/settings', body: { host_port: 9090, auto_upgrade: false } },
    ])
    expect(JSON.parse(run.stdout).host_port).toBe(9090)
  })

  test('configure --host-port on a running gateway asks first, and refuses without a terminal', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'configure', '--host-port', '9090'],
      { 'GET /preview-gateway/settings': { ...settings, enabled: true, host_port: 8080 }, 'PATCH /preview-gateway/settings': settings },
      { stdin: '' },
    )
    expect(run.error?.message).toMatch(/not an interactive terminal/)
    expect(run.requests).toEqual(['GET /preview-gateway/settings'])
  })

  test('configure --disable without a terminal refuses instead of changing anything', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'configure', '--disable'],
      { 'PATCH /preview-gateway/settings': settings },
      { stdin: '' },
    )
    expect(run.error?.message).toMatch(/not an interactive terminal/)
    expect(run.requests).toEqual([])
  })

  test('configure --disable --yes disables it', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'configure', '--disable', '--yes'],
      { 'PATCH /preview-gateway/settings': { ...settings, enabled: false } },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies[0]?.body).toEqual({ enabled: false })
  })

  test('configure without flags fails before calling the API', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'configure'], {})
    expect(run.error?.message).toMatch(/Nothing to change/)
    expect(run.requests).toEqual([])
  })

  test('configure surfaces a refusal from the server', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'configure', '--container-name', 'other', '--yes'],
      {
        'PATCH /preview-gateway/settings': Response.json(
          { title: 'Conflict', detail: 'This host has sandboxes; their networks keep the current name' },
          { status: 409 },
        ),
      },
    )
    expect(run.error?.message).toMatch(/Could not save the preview gateway settings/)
    expect(run.error?.message).toMatch(/keep the current name/)
  })

  test('logs passes --tail and prints the lines', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'logs', '--tail', '5'], {
      'GET /preview-gateway/logs': { lines: ['line one', 'line two'] },
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['GET /preview-gateway/logs?tail=5'])
    expect(run.stdout).toContain('line one')
  })

  test('restart --yes restarts it', async () => {
    const run = await runWithApi(registerPreviewGatewayCommands, ['preview-gateway', 'restart', '--yes'], {
      'POST /preview-gateway/restart': new Response(null, { status: 204 }),
    })
    expect(run.error).toBeUndefined()
    expect(run.requests).toEqual(['POST /preview-gateway/restart'])
  })

  test('restart without --yes and no terminal does not restart', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'restart'],
      { 'POST /preview-gateway/restart': new Response(null, { status: 204 }) },
      { stdin: '' },
    )
    expect(run.error?.message).toMatch(/not an interactive terminal/)
    expect(run.requests).toEqual([])
  })

  test('upgrade --default --yes sends the empty image', async () => {
    const run = await runWithApi(
      registerPreviewGatewayCommands,
      ['preview-gateway', 'upgrade', '--default', '--yes'],
      { 'POST /preview-gateway/upgrade': new Response(null, { status: 204 }) },
    )
    expect(run.error).toBeUndefined()
    expect(run.bodies).toEqual([{ request: 'POST /preview-gateway/upgrade', body: { image: '' } }])
  })
})

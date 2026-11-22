// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import {
  MicrosandboxBackendOption,
  type MicrosandboxCapability,
} from './MicrosandboxBackendOption'

const missingRuntime: MicrosandboxCapability = {
  configured: false,
  reason:
    'microsandbox runtime v0.7.7 (msb + libkrunfw) is not installed under /data/microsandbox',
  setup_path: '/agent-sandbox/sandbox',
  setup_command: 'temps microsandbox setup',
  runtime_version: '0.7.7',
  terminal: { supported: true, reason: null, setup_command: null },
}

function render(capability: MicrosandboxCapability | undefined) {
  return renderToStaticMarkup(
    <MicrosandboxBackendOption
      capability={capability}
      selected={false}
      onSelect={() => {}}
    />
  )
}

describe('MicrosandboxBackendOption', () => {
  test('stays visible and onboards when the runtime is missing', () => {
    const html = render(missingRuntime)
    expect(html).toContain('microsandbox')
    expect(html).toContain('Experimental')
    expect(html).toContain('Not available on this host')
    expect(html).toContain('is not installed under /data/microsandbox')
    expect(html).toContain('temps microsandbox setup')
    expect(html).toContain('disabled=""')
  })

  test('offers no setup command when installing cannot fix the host', () => {
    const html = render({
      ...missingRuntime,
      reason: 'hardware virtualization is unavailable on this host: no kvm',
      setup_command: null,
    })
    expect(html).toContain('no kvm')
    expect(html).not.toContain('temps microsandbox setup')
  })

  test('is selectable once configured', () => {
    const html = render({
      ...missingRuntime,
      configured: true,
      reason: null,
      setup_command: null,
      setup_path: null,
    })
    expect(html).not.toContain('Not available')
    expect(html).not.toContain('disabled=""')
  })

  test('says terminals are unavailable on a ready backend that lacks the agent', () => {
    const html = render({
      ...missingRuntime,
      configured: true,
      reason: null,
      setup_command: null,
      setup_path: null,
      terminal: {
        supported: false,
        reason: 'the guest terminal agent (temps-pty-agent) is not staged',
        setup_command: 'temps microsandbox setup',
      },
    })
    // Still selectable: terminals are an add-on, not a gate on the backend.
    expect(html).not.toContain('disabled=""')
    expect(html).toContain('Terminals unavailable')
    expect(html).toContain('temps microsandbox setup')
  })

  test('does not claim unavailability while status is loading', () => {
    const html = render(undefined)
    expect(html).not.toContain('Not available')
  })
})

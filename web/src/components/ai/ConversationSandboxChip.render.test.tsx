// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { ConversationSandboxChip } from './ConversationSandboxChip'

describe('ConversationSandboxChip', () => {
  test('shows the sandbox and the logo of its backend', () => {
    const markup = renderToStaticMarkup(
      <ConversationSandboxChip name="scratch" backend="firecracker" />
    )
    expect(markup).toContain('Sandbox scratch · Firecracker')
    expect(markup).toContain('/sandbox-backends/firecracker.svg')
  })

  test('clips the microsandbox lockup to its mark', () => {
    const markup = renderToStaticMarkup(
      <ConversationSandboxChip name="scratch" backend="microsandbox" />
    )
    expect(markup).toContain('/sandbox-backends/microsandbox.svg')
    expect(markup).toContain('overflow-hidden')
  })

  test('docker uses the Docker mark', () => {
    const markup = renderToStaticMarkup(
      <ConversationSandboxChip name="scratch" backend="docker" />
    )
    expect(markup).toContain('/presets/docker.svg')
  })

  test('a sandbox with no recorded backend still shows, without a logo file', () => {
    const markup = renderToStaticMarkup(
      <ConversationSandboxChip name="scratch" backend={null} />
    )
    expect(markup).toContain('Sandbox scratch')
    expect(markup).not.toContain('<img')
  })
})

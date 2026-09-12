// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup as renderMarkup } from 'react-dom/server'
import type { ReactNode } from 'react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'

import type { ApplicationWorkspaceResponse } from '@/api/client'
import {
  ApplicationWorkspaceSettingsPanel,
  workspaceResourceFingerprint,
} from './ApplicationWorkspaceSettingsPanel'

function renderToStaticMarkup(children: ReactNode) {
  return renderMarkup(
    <QueryClientProvider client={new QueryClient()}>
      {children}
    </QueryClientProvider>
  )
}

const workspace: ApplicationWorkspaceResponse = {
  cpu_limit: 2,
  data_network_service_count: 0,
  desired_state: 'running',
  disk_limit_enforced: false,
  disk_limit_mb: 20_480,
  idle_timeout_secs: 86_400,
  image: 'ghcr.io/example/sandbox:1',
  memory_limit_mb: 4096,
  open_preview_ports: [],
  persistent_volume_healthy: true,
  pids_limit: 1024,
  runtime: 'node',
  runtime_update_available: false,
  sandbox_public_id: 'sbx_abcdef0123456789',
  state: 'running',
}

describe('ApplicationWorkspaceSettingsPanel', () => {
  test('uses a responsive settings grid for full-page workspace detail', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ApplicationWorkspaceSettingsPanel layout="page" applicationPublicId="app_example" initialWorkspace={workspace} />
      </MemoryRouter>
    )
    expect(html).toContain('lg:grid-cols-2')
    expect(html).not.toContain('max-w-')
    expect(html).toContain('Desired resources')
    expect(html).toContain('Harness maintenance')
  })
  test('status polling does not invalidate resource drafts, but configuration changes do', () => {
    expect(
      workspaceResourceFingerprint({
        ...workspace,
        memory_used_bytes: 123,
        state: 'running',
      })
    ).toBe(workspaceResourceFingerprint(workspace))
    expect(
      workspaceResourceFingerprint({ ...workspace, runtime: 'full' })
    ).not.toBe(workspaceResourceFingerprint(workspace))
    expect(
      workspaceResourceFingerprint({ ...workspace, cpu_limit: 8 })
    ).not.toBe(workspaceResourceFingerprint(workspace))
  })
  test('shows sandbox-specific harness maintenance commands and console link', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ApplicationWorkspaceSettingsPanel
          applicationPublicId="app_example"
          initialWorkspace={workspace}
        />
      </MemoryRouter>
    )

    expect(html).toContain('Harness maintenance')
    expect(html).toContain('claude update &amp;&amp; claude --version')
    expect(html).toContain('@openai/codex@latest')
    expect(html).toContain('opencode upgrade --method curl')
    expect(html).toContain('/workspaces/app_example')
    expect(html).toContain('Workspace details')
    expect(html).not.toContain('/sandboxes/sbx_abcdef0123456789')
    expect(html).toContain(
      'bunx @temps-sdk/cli sandbox shell sbx_abcdef0123456789'
    )
    expect(html).toContain('Run as a one-shot CLI command')
  })

  test('does not show maintenance commands before a sandbox exists', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ApplicationWorkspaceSettingsPanel
          applicationPublicId="app_example"
          initialWorkspace={{ ...workspace, sandbox_public_id: null }}
        />
      </MemoryRouter>
    )

    expect(html).not.toContain('Harness maintenance')
  })

  test('shows a failed startup reason and retry before lifecycle controls', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ApplicationWorkspaceSettingsPanel
          applicationPublicId="app_example"
          initialWorkspace={{
            ...workspace,
            state: 'failed',
            last_error:
              'This Temps host has no private sandbox network capacity available.',
          }}
        />
      </MemoryRouter>
    )

    expect(html).toContain('role="alert"')
    expect(html).toContain('Workspace could not start')
    expect(html).toContain(
      'This Temps host has no private sandbox network capacity available.'
    )
    expect(html).toContain('Try again')
    expect(html.indexOf('Workspace could not start')).toBeLessThan(
      html.indexOf('Lifecycle')
    )
  })
})

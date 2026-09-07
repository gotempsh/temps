// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type { ApplicationWorkspaceResponse } from '@/api/client'
import { ApplicationWorkspaceSettingsPanel } from './ApplicationWorkspaceSettingsPanel'

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
  sandbox_public_id: 'sbx_abcdef0123456789',
  state: 'running',
}

describe('ApplicationWorkspaceSettingsPanel', () => {
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
    expect(html).toContain('/sandboxes/sbx_abcdef0123456789')
    expect(html).toContain('temps sandbox shell sbx_abcdef0123456789')
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
})

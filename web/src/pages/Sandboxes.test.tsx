// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type {
  ApplicationResponse,
  ApplicationWorkspaceResponse,
} from '@/api/client'
import {
  ManagedApplicationWorkspaceRow,
  ManagedGlobalWorkspaceRow,
} from './Sandboxes'

describe('ManagedApplicationWorkspaceRow', () => {
  test('identifies a Temps-managed application sandbox without generic controls', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ManagedApplicationWorkspaceRow
          application={application}
          workspace={workspace}
        />
      </MemoryRouter>
    )

    expect(html).toContain('Managed by Temps')
    expect(html).toContain('Workspace topology')
    expect(html).toContain('sbx_workspace123')
    expect(html).toContain('node')
    expect(html).toContain('2 connected')
    expect(html).toContain('Persistent files')
    expect(html).toContain('Healthy')
    expect(html).toContain('/ai-first?application=app_workspace-topology-e2e')
    expect(html).toContain('Manage workspace')
    expect(html).not.toContain('>Stop<')
    expect(html).not.toContain('>Delete<')
    expect(html).not.toContain('Extend:')
  })

  test('explains that an idle managed workspace wakes automatically', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ManagedApplicationWorkspaceRow
          application={application}
          workspace={{ ...workspace, state: 'sleeping' }}
        />
      </MemoryRouter>
    )

    expect(html).toContain('sleeping · wakes automatically')
    expect(html).toContain(
      'The next AI turn, terminal, file, or preview request resumes this workspace.'
    )
  })

  test('explains when an application sandbox has not started yet', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ManagedApplicationWorkspaceRow
          application={application}
          workspace={{ ...workspace, sandbox_public_id: null }}
        />
      </MemoryRouter>
    )

    expect(html).toContain('not started')
    expect(html).toContain('Sandbox starts on the first application turn')
  })
})

describe('ManagedGlobalWorkspaceRow', () => {
  test('shows the shared AI sandbox without standalone lifecycle controls', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ManagedGlobalWorkspaceRow workspace={workspace} />
      </MemoryRouter>
    )

    expect(html).toContain('Global AI workspace')
    expect(html).toContain('sbx_workspace123')
    expect(html).toContain('/ai-first?scope=global')
    expect(html).toContain('Managed by Temps')
    expect(html).not.toContain('>Stop<')
    expect(html).not.toContain('>Delete<')
  })
})

const application: ApplicationResponse = {
  created_at: '2026-09-03T00:00:00Z',
  description: null,
  name: 'Workspace topology',
  projects: [],
  public_id: 'app_workspace-topology-e2e',
  status: 'active',
  updated_at: '2026-09-03T00:00:00Z',
}

const workspace: ApplicationWorkspaceResponse = {
  cpu_limit: 2,
  cpu_usage_usec: 1200,
  data_network_service_count: 2,
  desired_state: 'running',
  disk_limit_enforced: false,
  disk_limit_mb: 20_480,
  disk_used_bytes: 1024,
  idle_timeout_secs: 600,
  image: 'ghcr.io/gotempsh/temps-sandbox-node:0.1.0',
  last_error: null,
  memory_limit_mb: 4096,
  memory_used_bytes: 2048,
  open_preview_ports: [3000],
  persistent_volume_healthy: true,
  pids_limit: 1024,
  pids_used: 4,
  runtime: 'node',
  sandbox_public_id: 'sbx_workspace123',
  snapshot_id: null,
  state: 'running',
}

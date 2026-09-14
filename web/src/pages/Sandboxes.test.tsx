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
  ManagedApplicationWorkspaces,
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
    expect(html).toContain('/workspaces/app_workspace-topology-e2e')
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
    expect(html).toContain(
      'No compute attached. Workspace context is retained.'
    )
  })
})

describe('ManagedGlobalWorkspaceRow', () => {
  test('shows the shared AI sandbox without standalone lifecycle controls', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ManagedGlobalWorkspaceRow workspace={workspace} />
      </MemoryRouter>
    )

    expect(html).toContain('Default workspace')
    expect(html).toContain('sbx_workspace123')
    expect(html).toContain('/workspaces/global')
    expect(html).toContain('Managed by Temps')
    expect(html).not.toContain('>Stop<')
    expect(html).not.toContain('>Delete<')
  })
})

describe('workspace and operator views', () => {
  function render(computeOnly: boolean, attached: boolean, error = false) {
    return renderToStaticMarkup(
      <MemoryRouter>
        <ManagedApplicationWorkspaces
          entries={[
            {
              application,
              workspace: {
                ...workspace,
                sandbox_public_id: attached
                  ? workspace.sandbox_public_id
                  : null,
              },
            },
          ]}
          globalWorkspace={null}
          loading={false}
          error={error}
          computeOnly={computeOnly}
        />
      </MemoryRouter>
    )
  }

  test('retains working contexts without compute', () => {
    expect(render(false, false)).toContain('Workspace topology')
    expect(render(false, false)).toContain('No compute attached')
  })
  test('operator view includes only attached compute with an owner link', () => {
    expect(render(true, false)).not.toContain('Workspace topology')
    expect(render(true, false)).toContain(
      'No workspace-owned compute is attached'
    )
    expect(render(true, true)).toContain('Workspace-owned sandboxes')
    expect(render(true, true)).toContain(
      '/workspaces/app_workspace-topology-e2e'
    )
  })
  test('failed ownership lookups are not presented as an empty operator inventory', () => {
    expect(render(true, false, true)).toContain('could not be loaded')
    expect(render(true, false, true)).not.toContain(
      'No workspace-owned compute is attached'
    )
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
  runtime_update_available: false,
  sandbox_public_id: 'sbx_workspace123',
  snapshot_id: null,
  state: 'running',
}

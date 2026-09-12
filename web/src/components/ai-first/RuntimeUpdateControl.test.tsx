// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ApplicationWorkspaceResponse } from '@/api/client'
import {
  RuntimeUpdateControl,
  RUNTIME_UPDATE_WARNING,
} from './RuntimeUpdateControl'
import {
  workspaceNeedsAutomaticWake,
  workspaceStatusPresentation,
} from './workspace-readiness'

const workspace: ApplicationWorkspaceResponse = {
  state: 'running',
  desired_state: 'running',
  sandbox_public_id: 'sbx_example',
  runtime: 'node',
  image: null,
  runtime_compatible: false,
  runtime_update_available: true,
  runtime_update_image: 'managed-node:next',
  runtime_update_error:
    'The installed runtime does not support the required protocol.',
  cpu_limit: 2,
  memory_limit_mb: 4096,
  pids_limit: 512,
  disk_limit_mb: 20480,
  disk_limit_enforced: false,
  idle_timeout_secs: 900,
  memory_used_bytes: null,
  pids_used: null,
  disk_used_bytes: null,
  cpu_usage_usec: null,
  open_preview_ports: [],
  persistent_volume_healthy: true,
  data_network_service_count: 0,
  last_error: null,
  snapshot_id: null,
}

function render(next: ApplicationWorkspaceResponse) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <RuntimeUpdateControl
        applicationPublicId="app_example"
        workspace={next}
        onUpdated={() => {}}
      />
    </QueryClientProvider>
  )
}

describe('runtime updates', () => {
  test('shows a discoverable update action and mismatch explanation', () => {
    const html = render(workspace)
    expect(html).toContain('Runtime update required')
    expect(html).toContain('Update runtime')
    expect(html).toContain('restarting keeps the same image')
    expect(html).not.toContain('Confirm runtime update')
  })
  test('unconfigured updates remain visible with actionable guidance', () => {
    const html = render({ ...workspace, runtime_update_available: false })
    expect(html).toContain('Update runtime')
    expect(html).toContain('disabled')
    expect(html).toContain('instance administrator')
  })
  test('warns about process interruption and file preservation', () => {
    expect(RUNTIME_UPDATE_WARNING).toContain(
      'files and saved settings are preserved'
    )
    expect(RUNTIME_UPDATE_WARNING).toContain('Running processes will stop')
    expect(RUNTIME_UPDATE_WARNING).toContain('all active threads')
  })
  test('an incompatible healthy container is not ready and never auto-wakes', () => {
    expect(workspaceStatusPresentation(workspace, false).label).toBe(
      'Runtime update required'
    )
    expect(workspaceNeedsAutomaticWake({ ...workspace, state: 'failed' })).toBe(
      false
    )
  })
  test('failed compatibility probes never report ready', () => {
    expect(
      workspaceStatusPresentation(
        { ...workspace, runtime_compatible: null },
        false
      ).label
    ).toBe('Runtime unavailable')
  })
})

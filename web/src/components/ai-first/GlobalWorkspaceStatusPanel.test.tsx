// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { GlobalWorkspaceStatusPanel } from './GlobalWorkspaceStatusPanel'

describe('GlobalWorkspaceStatusPanel', () => {
  test('shows the managed sandbox identity, health, and configuration entry point', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <GlobalWorkspaceStatusPanel
          loading={false}
          waking={false}
          workspace={{
            state: 'running',
            desired_state: 'running',
            sandbox_public_id: 'sbx_abcdef0123456789',
            runtime: 'node',
            image: 'temps-agent:latest',
            cpu_limit: 4,
            memory_limit_mb: 8192,
            pids_limit: 512,
            disk_limit_mb: 10240,
            disk_limit_enforced: false,
            idle_timeout_secs: 900,
            memory_used_bytes: 1048576,
            pids_used: 4,
            disk_used_bytes: 2097152,
            cpu_usage_usec: 1500000,
            open_preview_ports: [],
            persistent_volume_healthy: true,
            data_network_service_count: 2,
            last_error: null,
            snapshot_id: null,
          }}
        />
      </MemoryRouter>
    )

    expect(html).toContain('User workspace')
    expect(html).toContain('Sandbox ready')
    expect(html).toContain('sbx_abcdef0123456789')
    expect(html).toContain('Persistent volume')
    expect(html).toContain('Healthy')
    expect(html).toContain('href="/agent-sandbox/sandbox"')
    expect(html).not.toContain('/sandboxes/sbx_abcdef0123456789')
  })
})

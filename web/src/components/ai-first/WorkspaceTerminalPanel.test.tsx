// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ApplicationWorkspaceResponse } from '@/api/client'
import {
  WorkspaceTerminalOnboarding,
  WorkspaceTerminalPanel,
  type WorkspaceTerminalPanelProps,
} from './WorkspaceTerminalPanel'
import type { WorkspaceTerminalAvailability } from './workspace-terminal'

const SANDBOX_ID = 'sbx_0123456789abcdef'

function workspace(
  overrides: Partial<ApplicationWorkspaceResponse> = {}
): ApplicationWorkspaceResponse {
  return {
    cpu_limit: 1,
    data_network_service_count: 0,
    desired_state: 'running',
    disk_limit_enforced: false,
    disk_limit_mb: 1024,
    idle_timeout_secs: 600,
    memory_limit_mb: 1024,
    open_preview_ports: [],
    persistent_volume_healthy: true,
    pids_limit: 256,
    runtime: 'docker',
    runtime_update_available: false,
    sandbox_public_id: SANDBOX_ID,
    state: 'running',
    ...overrides,
  }
}

function renderPanel(overrides: Partial<WorkspaceTerminalPanelProps> = {}) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <WorkspaceTerminalPanel
        applicationPublicId="app_123"
        threadPublicId="0123456789abcdef0123456789abcdef"
        loading={false}
        onOpenWorkspaceSettings={() => {}}
        onWake={() => {}}
        role="user"
        waking={false}
        workspace={workspace()}
        {...overrides}
      />
    </MemoryRouter>
  )
}

function renderOnboarding(availability: WorkspaceTerminalAvailability) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <WorkspaceTerminalOnboarding
        availability={availability}
        onOpenWorkspaceSettings={() => {}}
        onWake={() => {}}
        onWakeOnConnect={() => {}}
      />
    </MemoryRouter>
  )
}

describe('WorkspaceTerminalPanel', () => {
  test('a running workspace shows the terminal surface', () => {
    const html = renderPanel()
    expect(html).toContain('Shell in the workspace sandbox')
    expect(html).toContain('aria-label="Workspace terminal"')
    expect(html).toContain('Connecting…')
    expect(html).toContain('Extra keys')
    expect(html).toContain('Paste into terminal')
    // The terminal host is visible, not the onboarding card.
    expect(html).not.toContain('class="hidden"')
    expect(html).not.toContain('is sleeping')
    // The terminal is the way in; there is no CLI route for managed
    // workspaces, so none is advertised.
    expect(html).not.toContain('sandbox shell')
  })

  test('a sleeping workspace onboards with a wake action', () => {
    const html = renderPanel({ workspace: workspace({ state: 'sleeping' }) })
    expect(html).toContain('The workspace sandbox is sleeping')
    expect(html).toContain('Wake workspace')
    expect(html).not.toContain('Connecting…')
  })

  test('the global workspace is woken by connecting', () => {
    const html = renderPanel({
      applicationPublicId: null,
      workspace: workspace({ state: 'sleeping' }),
    })
    expect(html).toContain('Wake and connect')
  })

  test('a workspace with no sandbox yet is created by connecting', () => {
    for (const applicationPublicId of [null, 'app_demo']) {
      const html = renderPanel({
        applicationPublicId,
        workspace: workspace({ sandbox_public_id: null, state: 'sleeping' }),
      })
      expect(html).toContain('Create and connect')
      expect(html).toContain('hasn&#x27;t been created yet')
    }
  })

  test('a role without sandboxes:exec is told exactly what is missing', () => {
    const html = renderPanel({ role: 'platform_admin' })
    expect(html).toContain('Your account can&#x27;t open a terminal')
    expect(html).toContain('sandboxes:exec')
    expect(html).toContain('platform_admin')
    expect(html).toContain('href="/users"')
  })

  test('an incompatible runtime points at Workspace settings', () => {
    const html = renderPanel({
      workspace: workspace({
        runtime_compatible: false,
        runtime_update_error: 'The image predates the terminal agent.',
      }),
    })
    expect(html).toContain('The sandbox runtime needs an update')
    expect(html).toContain('The image predates the terminal agent.')
    expect(html).toContain('Open Workspace settings')
  })

  test('a backend without terminal support says why and links the fix', () => {
    const html = renderPanel({
      backendTerminal: {
        supported: false,
        reason: 'the guest terminal agent is not staged',
        setup_command: 'temps firecracker setup --pty-agent-bin <path>',
      },
      workspace: workspace({ runtime: 'firecracker' }),
    })
    expect(html).toContain(
      'Terminals aren&#x27;t available on this sandbox backend'
    )
    expect(html).toContain('the guest terminal agent is not staged')
    expect(html).toContain('temps firecracker setup --pty-agent-bin')
    expect(html).toContain('href="/agent-sandbox/sandbox"')
  })

  test('an archived terminal thread explains it must be restored', () => {
    const html = renderPanel({ archived: true })
    expect(html).toContain('This terminal thread is archived')
    expect(html).toContain('Restore this thread')
    expect(html).not.toContain('Connecting…')
  })

  test('loading the workspace shows a placeholder, not a blank panel', () => {
    const html = renderPanel({ workspace: null, loading: true })
    expect(html).toContain('Checking the workspace sandbox')
  })

  test('a waking workspace says it is starting', () => {
    const html = renderPanel({
      waking: true,
      workspace: workspace({ state: 'sleeping' }),
    })
    expect(html).toContain('Starting the workspace sandbox')
  })
})

describe('WorkspaceTerminalOnboarding', () => {
  test('every state shows a concrete example of what the terminal does', () => {
    const states: WorkspaceTerminalAvailability[] = [
      { kind: 'starting' },
      { kind: 'sleeping', wake: 'resume-action' },
      { kind: 'no-permission', role: 'reader' },
      { kind: 'runtime-update-required', detail: 'update' },
      { kind: 'backend-unsupported', reason: 'no', setupCommand: null },
      { kind: 'not-created' },
      { kind: 'failed', detail: 'disk full' },
    ]
    for (const state of states) {
      expect(renderOnboarding(state)).toContain('$ git status')
    }
  })

  test('a failed workspace surfaces the server diagnostic', () => {
    expect(
      renderOnboarding({
        kind: 'failed',
        detail: 'volume could not be mounted',
      })
    ).toContain('volume could not be mounted')
  })

  test('a ready state renders nothing of its own', () => {
    expect(
      renderOnboarding({ kind: 'ready', sandboxPublicId: SANDBOX_ID })
    ).toBe('')
  })
})

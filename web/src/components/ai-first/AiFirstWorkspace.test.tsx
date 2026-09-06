// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  ApplicationStartScreen,
  HarnessPicker,
  ThreadStatusIndicator,
  WorkspaceFilesPanel,
  WorkspaceStatusIndicator,
  WorkspaceViewTabs,
  mergeConversationPages,
} from './AiFirstWorkspace'
import { problemDetail } from './problem-detail'
import {
  workspaceHarnessOptions,
  workspaceStatusClickTarget,
  workspaceStatusPresentation,
} from './workspace-readiness'

describe('HarnessPicker', () => {
  test('does not render a partial provider inventory while detection is running', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <HarnessPicker
          harnesses={[
            {
              id: 'claude_cli',
              name: 'Claude Code',
              authMethod: 'subscription',
              models: [],
            },
          ]}
          loading
          onSelect={() => {}}
          selectedId="claude_cli"
        />
      </MemoryRouter>
    )

    expect(html).toContain('Checking installed harnesses')
    expect(html).not.toContain('Claude Code')
  })

  test('only exposes providers confirmed ready for persistent workspaces', () => {
    const base = {
      auth_command: 'login',
      auth_flavors: [],
      credential_saved: true,
      current_auth_type: 'subscription',
      default_model: null,
      default_permission_mode_id: 'default',
      default_runtime_model_id: null,
      host_auth_hint: null,
      host_auth_method: null,
      host_authenticated: true,
      host_version: null,
      install_command: 'install',
      max_turns_analysis: null,
      max_turns_feedback: null,
      max_turns_fix: null,
      model_source: 'bootstrap',
      models: [],
      models_refreshed_at: null,
      permission_modes: [],
      runtime_models: [],
      supports_max_turns: false,
      workspace_readiness_hint: null,
    }
    const harnesses = workspaceHarnessOptions([
      {
        ...base,
        id: 'claude_cli',
        name: 'Claude Code',
        workspace_ready: true,
      },
      {
        ...base,
        id: 'codex_cli',
        name: 'Codex',
        workspace_ready: false,
        workspace_readiness_hint: 'Secure workspace relay is not implemented.',
      },
    ])

    expect(harnesses.map((harness) => harness.id)).toEqual(['claude_cli'])
  })
})

describe('workspace pagination', () => {
  test('retains more than one hundred uniquely paged workspaces', () => {
    const first = Array.from({ length: 50 }, (_, index) => ({
      public_id: `app_${index}`,
    }))
    const rest = Array.from({ length: 76 }, (_, index) => ({
      public_id: `app_${index + 49}`,
    }))

    const merged = mergeConversationPages(first, rest)
    expect(merged).toHaveLength(125)
    expect(merged[merged.length - 1]?.public_id).toBe('app_124')
  })
})

describe('WorkspaceStatusIndicator', () => {
  const workspace = {
    state: 'running',
    desired_state: 'running',
    sandbox_public_id: 'sbx_123',
    runtime: 'node',
    image: null,
    cpu_limit: 4,
    memory_limit_mb: 8192,
    pids_limit: 512,
    disk_limit_mb: 10240,
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

  test('maps accessible, recovering, and sleeping sandboxes to explicit colors', () => {
    expect(workspaceStatusPresentation(workspace, false)).toMatchObject({
      label: 'Sandbox ready',
      dot: 'bg-emerald-500',
    })
    expect(
      workspaceStatusPresentation({ ...workspace, state: 'recovering' }, false)
    ).toMatchObject({ label: 'Sandbox starting' })
    expect(
      workspaceStatusPresentation({ ...workspace, state: 'sleeping' }, false)
    ).toMatchObject({ label: 'Sandbox sleeping', dot: 'bg-red-500' })
    expect(
      workspaceStatusPresentation(
        { ...workspace, state: 'sleeping' },
        false,
        true
      )
    ).toMatchObject({
      label: 'Sandbox waking',
      dot: 'animate-pulse bg-amber-500',
    })
  })

  test('renders the sandbox identity and accessibility description', () => {
    const html = renderToStaticMarkup(
      <WorkspaceStatusIndicator loading={false} workspace={workspace} />
    )
    expect(html).toContain('Sandbox ready')
    expect(html).toContain('sbx_123')
    expect(html).toContain('running and accessible')
  })

  test('opens the managed workspace panel without crossing into standalone sandbox routes', () => {
    expect(workspaceStatusClickTarget(true, workspace)).toBe('workspace')
    expect(workspaceStatusClickTarget(false, workspace)).toBe('workspace')
    expect(workspaceStatusClickTarget(false, null)).toBeNull()
  })
})

describe('ApplicationStartScreen', () => {
  test('offers bounded blank, local-folder, and credentialed Git starts', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <ApplicationStartScreen
          harnesses={[
            {
              id: 'codex_cli',
              name: 'Codex',
              authMethod: 'subscription',
              models: [],
            },
          ]}
          harnessesLoading={false}
          onCancel={() => {}}
          onCreated={() => {}}
        />
      </MemoryRouter>
    )

    expect(html).toContain('Blank project')
    expect(html).toContain('Local folder')
    expect(html).toContain('Git repository')
    expect(html).toContain('Autopack project')
  })
})

describe('ThreadStatusIndicator', () => {
  test('renders an accessible compact status for every supported state', () => {
    for (const [status, label] of [
      ['pending', 'Pending'],
      ['error', 'Error'],
      ['succeeded', 'Succeeded'],
    ] as const) {
      const html = renderToStaticMarkup(
        <ThreadStatusIndicator status={status} />
      )
      expect(html).toContain(`Thread status: ${label}`)
      expect(html).toContain(`>${label}</span>`)
    }
  })
})

describe('problemDetail', () => {
  test('surfaces RFC 7807 details returned by failed workspace creation', () => {
    expect(
      problemDetail(
        {
          title: 'Global Chat Sandbox Failed',
          detail: 'Sandbox provider docker is unavailable',
        },
        'Could not create workspace chat.'
      )
    ).toBe('Sandbox provider docker is unavailable')
  })
})

describe('WorkspaceViewTabs', () => {
  test('keeps the primary workspace surfaces in one fixed tab row', () => {
    const html = renderToStaticMarkup(
      <WorkspaceViewTabs
        activeView="files"
        changedFileCount={128}
        hasApplication
        onChange={() => {}}
      />
    )

    expect(html).toContain('grid-cols-4')
    expect(html).not.toContain('overflow-x-auto')
    expect(html).toContain('Output')
    expect(html).toContain('Preview')
    expect(html).toContain('Files')
    expect(html).toContain('Workspace')
    expect(html).not.toContain('Projects')
    expect(html).not.toContain('Settings')
    expect(html).toContain('99+')
    expect(html).toContain('aria-selected="true"')
  })

  test('keeps managed workspace status discoverable in the global view', () => {
    const html = renderToStaticMarkup(
      <WorkspaceViewTabs
        activeView="workspace"
        changedFileCount={0}
        hasApplication={false}
        onChange={() => {}}
      />
    )

    expect(html).toContain('grid-cols-2')
    expect(html).toContain('Output')
    expect(html).toContain('Workspace')
    expect(html).not.toContain('Preview')
    expect(html).not.toContain('Files')
    expect(html).toContain('aria-selected="true"')
  })
})

describe('WorkspaceFilesPanel', () => {
  test('renders the sandbox branch, changed files, diff, and tracked tree', () => {
    const html = renderToStaticMarkup(
      <WorkspaceFilesPanel
        changes={{
          branch: 'main',
          changes_truncated: false,
          head: 'abc123',
          clean: false,
          files_truncated: false,
          listed_file_count: 2,
          next_cursor: null,
          truncated: false,
          files: ['README.md', 'src/app.tsx'],
          changes: [
            {
              path: 'src/app.tsx',
              status: 'modified',
              staged: true,
              unstaged: false,
            },
          ],
        }}
        diff={{
          path: 'src/app.tsx',
          diff: 'diff --git a/src/app.tsx b/src/app.tsx\nindex 123..456 100644\n--- a/src/app.tsx\n+++ b/src/app.tsx\n@@ -4,2 +4,2 @@ export default function App()\n-old\n+new',
          truncated: false,
        }}
        diffLoading={false}
        error={null}
        fileCursor={0}
        loading={false}
        loadPhase="idle"
        onNextPage={() => {}}
        onOpenSettings={() => {}}
        onPreviousPage={() => {}}
        onRefresh={() => {}}
        onSelect={() => {}}
        selectedPath="src/app.tsx"
      />
    )

    expect(html).toContain('main')
    expect(html).toContain('HEAD abc123')
    expect(html).toContain('src/app.tsx')
    expect(html).toContain('staged')
    expect(html).toContain('old')
    expect(html).toContain('new')
    expect(html).toContain('+1')
    expect(html).toContain('−1')
    expect(html).not.toContain('diff --git')
    expect(html).toContain('README.md')
    expect(html).toContain('<span class="truncate">src/app.tsx</span>')
    expect(html).toContain('Git runs inside this application')
  })

  test('lists files even when every repository file is a working change', () => {
    const html = renderToStaticMarkup(
      <WorkspaceFilesPanel
        changes={{
          branch: 'main',
          changes_truncated: false,
          head: null,
          clean: false,
          files_truncated: false,
          listed_file_count: 1,
          next_cursor: null,
          truncated: false,
          files: ['src/new-file.ts'],
          changes: [
            {
              path: 'src/new-file.ts',
              status: 'untracked',
              staged: false,
              unstaged: true,
            },
          ],
        }}
        diff={null}
        diffLoading={false}
        error={null}
        fileCursor={0}
        loading={false}
        loadPhase="idle"
        onNextPage={() => {}}
        onOpenSettings={() => {}}
        onPreviousPage={() => {}}
        onRefresh={() => {}}
        onSelect={() => {}}
        selectedPath={null}
      />
    )

    expect(html).toContain('Files')
    expect(html).toContain('src/new-file.ts')
    expect(html).toContain('<span class="truncate">src/new-file.ts</span>')
  })

  test('renders bounded file-page navigation and communicates server caps', () => {
    const html = renderToStaticMarkup(
      <WorkspaceFilesPanel
        changes={{
          branch: 'main',
          head: 'abc123',
          clean: true,
          truncated: true,
          files_truncated: true,
          changes_truncated: true,
          listed_file_count: 1_000,
          next_cursor: 200,
          files: ['src/page-101.ts'],
          changes: [],
        }}
        diff={null}
        diffLoading={false}
        error={null}
        fileCursor={100}
        loading={false}
        loadPhase="idle"
        onNextPage={() => {}}
        onOpenSettings={() => {}}
        onPreviousPage={() => {}}
        onRefresh={() => {}}
        onSelect={() => {}}
        selectedPath={null}
      />
    )

    expect(html).toContain('101–101 of 1000+')
    expect(html).toContain('Previous')
    expect(html).toContain('Next')
    expect(html).toContain('first 1,000 safe file paths')
    expect(html).toContain('first 200 safe paths')
  })

  test('shows durable wake state instead of an unbounded Git loader', () => {
    const html = renderToStaticMarkup(
      <WorkspaceFilesPanel
        changes={null}
        diff={null}
        diffLoading={false}
        error={null}
        fileCursor={0}
        loading
        loadPhase="waking"
        onNextPage={() => {}}
        onOpenSettings={() => {}}
        onPreviousPage={() => {}}
        onRefresh={() => {}}
        onSelect={() => {}}
        selectedPath={null}
      />
    )

    expect(html).toContain('Waking the persistent workspace')
    expect(html).not.toContain('Inspecting Git inside the sandbox')
  })
})

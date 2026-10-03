// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  QueryClient,
  QueryClientProvider,
  type UseQueryResult,
} from '@tanstack/react-query'
import type {
  NodeSandboxesResponse,
  PlacementNode,
  SandboxInner,
  UserResponse,
} from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { EvictionReportAlert, NodeSandboxesPanel } from './NodeSandboxesPanel'
import {
  evictionReportFromProblem,
  evictionReportFromResponse,
  evictionReportNeedsAttention,
  isEvictionInProgress,
  withHttpStatus,
} from './node-eviction'

const ME = 7

function wrap(children: ReactNode, role = 'admin') {
  const user = { id: ME, role } as unknown as UserResponse
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <AuthContext.Provider
        value={{
          user,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter>{children}</MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

const node: PlacementNode = {
  id: 3,
  name: 'worker-1',
  is_control_plane: false,
  status: 'active',
  allowed: true,
  eligible: true,
  reason: null,
  live_sandboxes: 2,
}

function sandbox(id: string, extra: Partial<SandboxInner> = {}): SandboxInner {
  return {
    id,
    name: id,
    status: 'running',
    cwd: '/workspace',
    createdAt: Date.now() - 60_000,
    requestedAt: Date.now() - 60_000,
    updatedAt: Date.now() - 60_000,
    timeout: 1_800_000,
    lifecycle: 'ephemeral',
    memory: 512,
    vcpus: 1,
    region: 'local',
    runtime: 'node',
    preview_url_template: '',
    node_id: 3,
    node_name: 'worker-1',
    image: 'node:22',
    ...extra,
  }
}

function fakeQuery(
  state: Partial<UseQueryResult<NodeSandboxesResponse, Error>>
): UseQueryResult<NodeSandboxesResponse, Error> {
  return {
    data: undefined,
    error: null,
    isLoading: false,
    isError: false,
    refetch: async () => ({}),
    ...state,
  } as unknown as UseQueryResult<NodeSandboxesResponse, Error>
}

function renderPanel(
  query: UseQueryResult<NodeSandboxesResponse, Error>,
  canSee = true
) {
  return wrap(
    <NodeSandboxesPanel
      nodeId={3}
      nodeName="worker-1"
      canSee={canSee}
      query={query}
      page={1}
      onPageChange={() => {}}
    />
  )
}

describe('NodeSandboxesPanel', () => {
  test('explains to non-admins why the list is hidden', () => {
    const html = renderPanel(fakeQuery({ isLoading: true }), false)
    expect(html).toContain('Only administrators can see the sandboxes on a node')
    expect(html).not.toContain('Destroy all')
  })

  test('loads with skeleton rows shaped like the table, not a spinner', () => {
    const html = renderPanel(fakeQuery({ isLoading: true }))
    expect(html.match(/node-sandbox-skeleton-row/g)).toHaveLength(3)
    expect(html).toContain('animate-pulse')
    expect(html).not.toContain('animate-spin')
  })

  test('onboards an empty eligible node with the create command', () => {
    const html = renderPanel(
      fakeQuery({
        data: { node, sandboxes: [], total: 0, page: 1, page_size: 20 },
      })
    )
    expect(html).toContain('No sandboxes on this node.')
    expect(html).toContain('sandbox create --node worker-1')
    expect(html).not.toContain('Destroy all')
  })

  test('links only the sandboxes the viewer owns, and keeps hidden columns readable on mobile', () => {
    const html = renderPanel(
      fakeQuery({
        data: {
          node,
          sandboxes: [
            {
              sandbox: sandbox('sbx_mine', { lifecycle: 'workspace' }),
              owner_user_id: ME,
              owner_email: 'me@example.com',
            },
            {
              sandbox: sandbox('sbx_theirs', { image: null }),
              owner_user_id: 99,
              owner_email: 'other@example.com',
            },
          ],
          total: 2,
          page: 1,
          page_size: 20,
        },
      })
    )
    expect(html).toContain('href="/sandboxes/sbx_mine"')
    expect(html).not.toContain('href="/sandboxes/sbx_theirs"')
    expect(html).toContain('other@example.com')
    expect(html).toContain('Destroy all')
    // Stacked secondary text carries kind / created / image below md / lg.
    expect(html).toContain('Workspace · created')
    expect(html).toContain('platform default')
  })

  test('uses the shared pagination when the list spans pages', () => {
    const html = renderPanel(
      fakeQuery({
        data: {
          node,
          sandboxes: [
            { sandbox: sandbox('sbx_1'), owner_user_id: ME, owner_email: null },
          ],
          total: 45,
          page: 1,
          page_size: 20,
        },
      })
    )
    expect(html).toContain('aria-label="Sandboxes on this node"')
    expect(html).toContain('1 / 3')
  })

  test('shows why an ineligible node does not take sandboxes', () => {
    const html = renderPanel(
      fakeQuery({
        data: {
          node: {
            ...node,
            eligible: false,
            reason: 'its sandboxes are being destroyed',
          },
          sandboxes: [],
          total: 0,
          page: 1,
          page_size: 20,
        },
      })
    )
    expect(html).toContain(
      'This node does not accept new sandboxes: its sandboxes are being destroyed'
    )
  })
})

describe('eviction report', () => {
  const partialProblem = withHttpStatus(
    {
      type: 'https://temps.sh/probs/sandbox-node-eviction-incomplete',
      title: 'Sandbox Node Eviction Incomplete',
      detail: 'Destroyed 1 sandbox(es) on node worker-1, but 1 could not be destroyed.',
      destroyed: ['sbx_a'],
      containers_unconfirmed: [
        {
          sandbox_id: 'sbx_b',
          reason: 'node did not answer',
          cleanup_command: 'docker rm -f temps-sandbox-sbx_b',
        },
      ],
      failed: [{ sandbox_id: 'sbx_c', reason: 'database busy' }],
    },
    503
  )

  test('reads the 503 extension members', () => {
    const report = evictionReportFromProblem(partialProblem)
    expect(report).not.toBeNull()
    expect(report!.partial).toBe(true)
    expect(report!.destroyed).toEqual(['sbx_a'])
    expect(report!.failed).toEqual([
      { sandbox_id: 'sbx_c', reason: 'database busy' },
    ])
    expect(report!.containersUnconfirmed[0]?.cleanup_command).toBe(
      'docker rm -f temps-sandbox-sbx_b'
    )
  })

  test('falls back to the detail text when the problem has no members', () => {
    const report = evictionReportFromProblem({
      status: 503,
      type: 'https://temps.sh/probs/sandbox-node-eviction-incomplete',
      detail: 'Destroyed 2 sandbox(es), but 1 could not be destroyed.',
    })
    expect(report?.detail).toContain('could not be destroyed')
    const html = wrap(
      <EvictionReportAlert report={report!} onDismiss={() => {}} />
    )
    expect(html).toContain('Destroyed 2 sandbox(es), but 1 could not be destroyed.')
  })

  test('ignores errors that are not a partial eviction', () => {
    expect(evictionReportFromProblem(withHttpStatus({ detail: 'x' }, 500))).toBeNull()
    // A 503 for another reason destroyed nothing.
    expect(
      evictionReportFromProblem(withHttpStatus({ detail: 'sandbox subsystem is down' }, 503))
    ).toBeNull()
    expect(evictionReportFromProblem(undefined)).toBeNull()
  })

  test('recognises an eviction already in progress (409)', () => {
    expect(isEvictionInProgress(withHttpStatus({ detail: 'busy' }, 409))).toBe(true)
    expect(isEvictionInProgress(partialProblem)).toBe(false)
  })

  test('keeps a server-sent status over the HTTP one', () => {
    expect(withHttpStatus({ status: 409 }, 500).status).toBe(409)
    expect(withHttpStatus('plain text', 502)).toEqual({
      detail: 'plain text',
      status: 502,
    })
  })

  test('renders a partial eviction like the success path, with copyable cleanup commands', () => {
    const report = evictionReportFromProblem(partialProblem)!
    const html = wrap(<EvictionReportAlert report={report} onDismiss={() => {}} />)
    expect(html).toContain('Destroyed 1 sandbox(es), but not all of them.')
    expect(html).toContain('Could not destroy 1 sandbox(es):')
    expect(html).toContain('sbx_c')
    expect(html).toContain('database busy')
    expect(html).toContain('docker rm -f temps-sandbox-sbx_b')
    expect(html).toContain('Copy the command that removes sbx_b')
    // The structured members replace the long sentence.
    expect(html).not.toContain('but 1 could not be destroyed.')
  })

  test('a clean success needs no report; unconfirmed containers do', () => {
    expect(
      evictionReportNeedsAttention(
        evictionReportFromResponse({
          node,
          destroyed: ['sbx_a'],
          containers_unconfirmed: [],
        })
      )
    ).toBe(false)
    const withLeftovers = evictionReportFromResponse({
      node,
      destroyed: ['sbx_a'],
      containers_unconfirmed: [
        {
          sandbox_id: 'sbx_a',
          reason: 'timed out',
          cleanup_command: 'docker rm -f temps-sandbox-sbx_a',
        },
      ],
    })
    expect(evictionReportNeedsAttention(withLeftovers)).toBe(true)
    const html = wrap(
      <EvictionReportAlert report={withLeftovers} onDismiss={() => {}} />
    )
    expect(html).toContain('docker rm -f temps-sandbox-sbx_a')
    expect(html).not.toContain('not all of them')
  })
})

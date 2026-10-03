// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ReactElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type {
  PlacementNode,
  SandboxPlacementResponse,
  UserResponse,
} from '@/api/client'
import { getSandboxPlacementQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { SandboxNodesCard } from './SandboxNodesCard'
import { SandboxNodeBadge, SandboxNodeValue } from './SandboxNode'
import {
  allowedNodeIdsFromForm,
  placementExcludesControlPlane,
  placementFormDirty,
  placementFormValues,
} from './sandbox-placement'

const controlPlane: PlacementNode = {
  id: 0,
  name: 'control-plane',
  is_control_plane: true,
  status: 'active',
  allowed: true,
  eligible: true,
  reason: null,
  live_sandboxes: 1,
}
const worker: PlacementNode = {
  id: 3,
  name: 'worker-1',
  is_control_plane: false,
  status: 'active',
  allowed: true,
  eligible: true,
  reason: null,
  live_sandboxes: 0,
}

function renderCard(
  placement: SandboxPlacementResponse | undefined,
  role = 'admin'
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  if (placement) client.setQueryData(getSandboxPlacementQueryKey(), placement)
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user: { id: 1, role } as unknown as UserResponse,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter>
          <SandboxNodesCard />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

describe('SandboxNodesCard', () => {
  test('is read-only for non-admins and says why', () => {
    const html = renderCard(
      { allowed_node_ids: null, nodes: [controlPlane, worker] },
      'user'
    )
    expect(html).toContain(
      'Only administrators can change which nodes run sandboxes.'
    )
    // Every checkbox and the Save button are disabled.
    expect(html.match(/role="checkbox"[^>]*disabled=""/g)?.length).toBe(3)
    expect(html).toMatch(/type="submit" disabled="">Save<\/button>/)
  })

  test('onboards a single-node install towards adding a worker', () => {
    const html = renderCard({ allowed_node_ids: null, nodes: [controlPlane] })
    expect(html).toContain('Only the control plane is available.')
    expect(html).toContain('temps join')
    expect(html).toContain('href="/settings/nodes"')
  })

  test('warns what changes when the control plane is left out', () => {
    const html = renderCard({ allowed_node_ids: [3], nodes: [controlPlane, worker] })
    expect(html).toContain('The control plane will not take new sandboxes')
    expect(html).toContain('Fleet')
    expect(html).toContain('snapshots')
    expect(html).toContain('Managed AI application workspaces still always run')
  })

  test('no control-plane warning while every node is allowed', () => {
    const html = renderCard({ allowed_node_ids: null, nodes: [controlPlane, worker] })
    expect(html).not.toContain('The control plane will not take new sandboxes')
  })

  test('shows why a node does not take new sandboxes', () => {
    const html = renderCard({
      allowed_node_ids: null,
      nodes: [
        controlPlane,
        { ...worker, eligible: false, reason: 'node address is plain http' },
      ],
    })
    expect(html).toContain('Not taking new sandboxes: node address is plain http')
  })

  test('loads with skeleton rows, not text', () => {
    const html = renderCard(undefined)
    expect(html).not.toContain('Loading nodes')
    expect(html.match(/sandbox-node-skeleton-row/g)).toHaveLength(2)
  })
})

describe('sandbox placement form', () => {
  const data: SandboxPlacementResponse = {
    allowed_node_ids: [3, 0, 42],
    nodes: [controlPlane, worker],
  }

  test('drops ids of removed nodes and sorts the selection', () => {
    expect(placementFormValues(data)).toEqual({ allowAll: false, selected: [0, 3] })
    expect(
      placementFormValues({ allowed_node_ids: null, nodes: [controlPlane, worker] })
    ).toEqual({ allowAll: true, selected: [0, 3] })
  })

  test('always sends allowed_node_ids explicitly', () => {
    expect(allowedNodeIdsFromForm({ allowAll: true, selected: [3] })).toBeNull()
    expect(allowedNodeIdsFromForm({ allowAll: false, selected: [3, 0] })).toEqual([0, 3])
    expect(allowedNodeIdsFromForm({ allowAll: false, selected: [] })).toEqual([])
  })

  test('dirty compares sets, not order', () => {
    const saved = { allowed_node_ids: [3, 0], nodes: [controlPlane, worker] }
    expect(placementFormDirty(saved, { allowAll: false, selected: [0, 3] })).toBe(false)
    expect(placementFormDirty(saved, { allowAll: false, selected: [3] })).toBe(true)
    expect(placementFormDirty(saved, { allowAll: true, selected: [0, 3] })).toBe(true)
    expect(
      placementFormDirty(
        { allowed_node_ids: null, nodes: [controlPlane] },
        { allowAll: true, selected: [] }
      )
    ).toBe(false)
  })

  test('flags a selection without the control plane', () => {
    expect(placementExcludesControlPlane({ allowAll: false, selected: [3] })).toBe(true)
    expect(placementExcludesControlPlane({ allowAll: false, selected: [0, 3] })).toBe(false)
    expect(placementExcludesControlPlane({ allowAll: true, selected: [3] })).toBe(false)
    // Nothing selected has its own "nobody can create sandboxes" message.
    expect(placementExcludesControlPlane({ allowAll: false, selected: [] })).toBe(false)
  })
})

describe('sandbox node display', () => {
  const render = (node: ReactElement) =>
    renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>)

  test('the list badge only appears for worker sandboxes', () => {
    expect(
      render(
        <SandboxNodeBadge
          sandbox={{ node_id: null, node_name: 'control-plane' }}
          canOpenNode
        />
      )
    ).toBe('')
  })

  test('the list badge links to the node for admins only', () => {
    const admin = render(
      <SandboxNodeBadge sandbox={{ node_id: 3, node_name: 'worker-1' }} canOpenNode />
    )
    expect(admin).toContain('href="/settings/nodes/3"')
    expect(admin).toContain('worker-1')
    const user = render(
      <SandboxNodeBadge
        sandbox={{ node_id: 3, node_name: 'worker-1' }}
        canOpenNode={false}
      />
    )
    expect(user).not.toContain('href=')
    expect(user).toContain('worker-1')
  })

  test('the detail fact names the control plane or the worker', () => {
    expect(
      render(
        <SandboxNodeValue
          sandbox={{ node_id: null, node_name: 'control-plane' }}
          canOpenNode
        />
      )
    ).toBe('Control plane')
    expect(
      render(
        <SandboxNodeValue sandbox={{ node_id: 3, node_name: 'worker-1' }} canOpenNode />
      )
    ).toContain('href="/settings/nodes/3"')
    expect(
      render(
        <SandboxNodeValue
          sandbox={{ node_id: 3, node_name: 'worker-1' }}
          canOpenNode={false}
        />
      )
    ).toBe('worker-1')
  })
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import {
  attachEnvironmentVolume,
  attachVolumeFormSchema,
  createVolumeFormSchema,
  createEnvironmentVolume,
  createVolumeBody,
  deleteEnvironmentVolume,
  detachEnvironmentVolume,
  explainPlacement,
  getPinningRecommendation,
  getWorkloadIsolation,
  isolationSummary,
  isPermissionDenied,
  listEnvironmentVolumes,
  mountPathError,
  nodeLabel,
  nodeNamesOf,
  placementSummary,
  recommendationSummary,
  setWorkloadIsolation,
  setWorkloadMode,
  toCreateVolumeRequest,
  volumeDeleteConsequence,
  volumeDeletedMessage,
  volumeNameError,
  workloadModeSummary,
  type EnvironmentVolume,
  type PinningRecommendation,
  type PlacementExplanation,
} from './environment-workload'

afterEach(() => restoreAllMockApis())

function explanation(
  overrides: Partial<PlacementExplanation> = {}
): PlacementExplanation {
  return {
    environment_id: 3,
    recommended: [
      {
        node_id: 0,
        node_name: 'primary',
        score: 0.9,
        reasons: ['least loaded'],
      },
      { node_id: 4, node_name: 'worker-a', score: 0.4, reasons: [] },
    ],
    excluded: [
      {
        node_id: 5,
        node_name: 'worker-b',
        reason: 'NodeDraining',
        message: 'node 5 is draining',
        excluded: true,
      },
    ],
    placement: [0],
    unschedulable: null,
    unschedulable_reason: null,
    warnings: [],
    pinned_by: null,
    ...overrides,
  }
}

function recommendation(
  overrides: Partial<PinningRecommendation> = {}
): PinningRecommendation {
  return {
    environment_id: 3,
    window_days: 7,
    nodes_used: [0],
    moved_between_nodes: false,
    recommended_nodes: [],
    reasons: [],
    current_nodes: [],
    changes_placement: false,
    ...overrides,
  }
}

function volume(overrides: Partial<EnvironmentVolume> = {}): EnvironmentVolume {
  return {
    id: 9,
    environment_id: 3,
    name: 'data',
    backing: 'named',
    node_id: 0,
    size_mb: null,
    retain_on_delete: false,
    attachment: { mount_path: '/data', read_only: false },
    pin_reason: 'pinned to node 0 by volume `data`',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  }
}

describe('requests', () => {
  test('sets the workload mode with the server field name', async () => {
    let body: unknown
    const api = mockApi({
      'PUT /internal/environments/3/workload-mode': async (request) => {
        body = await request.json()
        return jsonResponse({ environment_id: 3, workload_mode: 'enforce' })
      },
    })
    const saved = await setWorkloadMode(3, 'enforce')
    expect(body).toEqual({ workload_mode: 'enforce' })
    expect(saved.workload_mode).toBe('enforce')
    expect(api.called('PUT', '/internal/environments/3/workload-mode')).toBe(
      true
    )
  })

  test('reads and sets the isolation', async () => {
    let body: unknown
    mockApi({
      'GET /internal/environments/3/workload-isolation': () =>
        jsonResponse({
          environment_id: 3,
          workload_isolation: 'process',
          applies: false,
        }),
      'PUT /internal/environments/3/workload-isolation': async (request) => {
        body = await request.json()
        return jsonResponse({
          environment_id: 3,
          workload_isolation: 'micro_vm',
          applies: true,
        })
      },
    })
    expect((await getWorkloadIsolation(3)).applies).toBe(false)
    const saved = await setWorkloadIsolation(3, 'micro_vm')
    expect(body).toEqual({ workload_isolation: 'micro_vm' })
    expect(saved.workload_isolation).toBe('micro_vm')
  })

  test('explains placement and passes the recommendation window', async () => {
    let query = ''
    mockApi({
      'GET /internal/environments/3/placement/explain': () =>
        jsonResponse(explanation()),
      'GET /internal/environments/3/placement/recommendation': (request) => {
        query = new URL(request.url).search
        return jsonResponse(recommendation({ window_days: 14 }))
      },
    })
    expect((await explainPlacement(3)).placement).toEqual([0])
    expect((await getPinningRecommendation(3, 14)).window_days).toBe(14)
    expect(query).toBe('?window_days=14')
  })

  test('lists volumes with page and page size', async () => {
    let query = ''
    mockApi({
      'GET /internal/environments/3/volumes': (request) => {
        query = new URL(request.url).search
        return jsonResponse({
          volumes: [volume()],
          total: 1,
          page: 2,
          page_size: 20,
        })
      },
    })
    const list = await listEnvironmentVolumes(3, 2, 20)
    expect(query).toBe('?page=2&page_size=20')
    expect(list.volumes[0].name).toBe('data')
  })

  test('creates, attaches, detaches and deletes a volume', async () => {
    const bodies: unknown[] = []
    const api = mockApi({
      'POST /internal/environments/3/volumes': async (request) => {
        bodies.push(await request.json())
        return jsonResponse(volume({ attachment: null }), 201)
      },
      'PUT /internal/environments/3/volumes/9/attachment': async (request) => {
        bodies.push(await request.json())
        return jsonResponse(volume())
      },
      'DELETE /internal/environments/3/volumes/9/attachment': () =>
        jsonResponse(volume({ attachment: null, pin_reason: null })),
      'DELETE /internal/environments/3/volumes/9': () =>
        jsonResponse({
          volume: volume({ attachment: null }),
          backing_removed: false,
          backing_kept_reason: 'retain_on_delete is set',
        }),
    })
    await createEnvironmentVolume(3, {
      name: ' data ',
      size_mb: 512,
      retain_on_delete: true,
    })
    await attachEnvironmentVolume(3, 9, { mount_path: ' /data ' })
    const detached = await detachEnvironmentVolume(3, 9)
    const deleted = await deleteEnvironmentVolume(3, 9)
    expect(bodies).toEqual([
      { name: 'data', size_mb: 512, retain_on_delete: true },
      { mount_path: '/data', read_only: false },
    ])
    expect(detached.attachment).toBeNull()
    expect(deleted.backing_kept_reason).toBe('retain_on_delete is set')
    expect(
      api.called('DELETE', '/internal/environments/3/volumes/9/attachment')
    ).toBe(true)
  })

  test('a refused read is recognised as a permission error', async () => {
    mockApi({
      'GET /internal/environments/3/placement/explain': () =>
        jsonResponse(
          { title: 'Forbidden', detail: 'Requires settings:read' },
          403
        ),
    })
    const error = await explainPlacement(3).catch((e: unknown) => e)
    expect(isPermissionDenied(error)).toBe(true)
    expect(isPermissionDenied(new Error('network'))).toBe(false)
  })
})

describe('createVolumeBody', () => {
  test('drops a size that is not a positive whole number', () => {
    expect(createVolumeBody({ name: 'data', size_mb: 0 })).toEqual({
      name: 'data',
      size_mb: null,
      retain_on_delete: false,
    })
    expect(createVolumeBody({ name: 'data', size_mb: 1.5 }).size_mb).toBeNull()
  })
})

describe('validation', () => {
  test('volume names follow the server rule', () => {
    expect(volumeNameError('data')).toBeNull()
    expect(volumeNameError('pg_data.v2-x')).toBeNull()
    expect(volumeNameError('d')).toContain('2 to 64')
    expect(volumeNameError('Data')).toContain('lowercase')
    expect(volumeNameError('-data')).toContain('starting with')
    expect(volumeNameError('a'.repeat(65))).toContain('2 to 64')
  })

  test('mount paths must be absolute without dot segments', () => {
    expect(mountPathError('/data')).toBeNull()
    expect(mountPathError('data')).toContain('absolute')
    expect(mountPathError('/')).toContain('root')
    expect(mountPathError('/data/../etc')).toContain('".."')
  })
})

describe('summaries', () => {
  test('each mode says what it does', () => {
    expect(workloadModeSummary('legacy')).toContain('exactly as before')
    expect(workloadModeSummary('shadow')).toContain('disagreement is logged')
    expect(workloadModeSummary('enforce')).toContain(
      'created and run by the workload plane'
    )
  })

  test('isolation is recorded but ignored outside enforce', () => {
    expect(
      isolationSummary({
        environment_id: 3,
        workload_isolation: 'micro_vm',
        applies: false,
      })
    ).toContain('ignored until the workload mode is Enforce')
    expect(
      isolationSummary({
        environment_id: 3,
        workload_isolation: 'micro_vm',
        applies: true,
      })
    ).toContain('Firecracker microVM')
  })

  test('names nodes, the control plane included', () => {
    const names = nodeNamesOf(explanation())
    expect(nodeLabel(0, names)).toBe('primary (control plane)')
    expect(nodeLabel(0, new Map())).toBe('Control plane')
    expect(nodeLabel(5, names)).toBe('worker-b')
    expect(nodeLabel(8, names)).toBe('Node 8')
  })

  test('placement says where the next deployment runs', () => {
    expect(placementSummary(explanation())).toBe(
      'The next deployment would run on primary (control plane).'
    )
    expect(placementSummary(explanation({ placement: [4, 4, 0] }))).toBe(
      "The next deployment's 3 replicas would run on worker-a (2 replicas), primary (control plane)."
    )
    expect(
      placementSummary(
        explanation({
          placement: null,
          unschedulable: 'no node provides a Firecracker microVM',
        })
      )
    ).toBe(
      'The next deployment could not be placed: no node provides a Firecracker microVM'
    )
  })

  test('recommendation names the nodes to pin to', () => {
    const names = nodeNamesOf(explanation())
    expect(
      recommendationSummary(
        recommendation({
          nodes_used: [0, 4],
          moved_between_nodes: true,
          recommended_nodes: [4],
          changes_placement: true,
        }),
        names
      )
    ).toBe(
      'Recommended: pin this environment to worker-a. It moved between nodes in the last 7 days.'
    )
    expect(
      recommendationSummary(recommendation({ nodes_used: [] }), names)
    ).toContain('nothing to recommend')
    expect(recommendationSummary(recommendation(), names)).toContain(
      'No change recommended'
    )
  })
})

describe('volume forms', () => {
  test('the create form maps to the request body', () => {
    const values = createVolumeFormSchema.parse({
      name: ' data ',
      sizeMb: '',
      retainOnDelete: true,
    })
    expect(toCreateVolumeRequest(values)).toEqual({
      name: 'data',
      size_mb: null,
      retain_on_delete: true,
    })
    expect(
      toCreateVolumeRequest(
        createVolumeFormSchema.parse({
          name: 'data',
          sizeMb: '1024',
          retainOnDelete: false,
        })
      ).size_mb
    ).toBe(1024)
  })

  test('the forms refuse what the server would refuse', () => {
    expect(
      createVolumeFormSchema.safeParse({
        name: 'Data',
        sizeMb: '',
        retainOnDelete: false,
      }).success
    ).toBe(false)
    expect(
      createVolumeFormSchema.safeParse({
        name: 'data',
        sizeMb: '-1',
        retainOnDelete: false,
      }).success
    ).toBe(false)
    expect(
      attachVolumeFormSchema.safeParse({ mountPath: 'data', readOnly: false })
        .success
    ).toBe(false)
    expect(
      attachVolumeFormSchema.safeParse({ mountPath: '/data', readOnly: true })
        .success
    ).toBe(true)
  })
})

describe('volume deletion', () => {
  test('says whether the data is retained', () => {
    expect(
      volumeDeleteConsequence(
        volume({ retain_on_delete: true, attachment: null })
      )
    ).toContain('its data is retained')
    expect(volumeDeleteConsequence(volume({ attachment: null }))).toBe(
      'Its Docker volume and all the data in it are removed from the control plane.'
    )
    expect(
      volumeDeleteConsequence(volume({ node_id: null, attachment: null }))
    ).toContain('no data to remove')
    expect(
      volumeDeleteConsequence(volume({ node_id: 4, attachment: null }))
    ).toContain('on node 4 is kept')
    expect(volumeDeleteConsequence(volume())).toContain('attached at /data')
  })

  test('the result message reports kept data with its reason', () => {
    expect(
      volumeDeletedMessage({
        volume: volume(),
        backing_removed: true,
        backing_kept_reason: null,
      })
    ).toBe('Deleted volume data and the data in it')
    expect(
      volumeDeletedMessage({
        volume: volume(),
        backing_removed: false,
        backing_kept_reason: 'retain_on_delete is set',
      })
    ).toBe('Deleted volume data; its data was kept: retain_on_delete is set')
  })
})

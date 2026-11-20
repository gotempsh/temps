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
  isPermissionDenied,
  listEnvironmentVolumes,
  mountPathError,
  toCreateVolumeRequest,
  volumeDeleteConsequence,
  volumeBlockerText,
  volumeDeletedMessage,
  volumeNameError,
  VOLUMES_REQUIREMENT,
  type EnvironmentVolume,
  type EnvironmentVolumeStatus,
} from './environment-workload'

afterEach(() => restoreAllMockApis())

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
      'GET /internal/environments/3/volumes': () =>
        jsonResponse(
          { title: 'Forbidden', detail: 'Requires settings:read' },
          403
        ),
    })
    const error = await listEnvironmentVolumes(3, 1, 20).catch(
      (e: unknown) => e
    )
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

describe('volume status', () => {
  const status = (
    overrides: Partial<EnvironmentVolumeStatus>
  ): EnvironmentVolumeStatus => ({
    environment_id: 3,
    workload_mode: 'enforce',
    replicas: 1,
    attached: 1,
    active: false,
    ...overrides,
  })

  test('never names the internal workload mode', () => {
    for (const workload_mode of ['legacy', 'shadow'] as const) {
      const text = volumeBlockerText(
        status({
          workload_mode,
          reason: `Environment 3 has workload_mode '${workload_mode}'; set workload_mode to enforce`,
        })
      )
      expect(text).toBe(
        "This environment's deployments do not mount volumes yet."
      )
      expect(text).not.toMatch(/workload|enforce|legacy|shadow/i)
    }
  })

  test('otherwise passes on the server reason, or the requirement', () => {
    expect(
      volumeBlockerText(status({ reason: 'Environment 3 runs 2 replicas' }))
    ).toBe('Environment 3 runs 2 replicas')
    expect(volumeBlockerText(status({}))).toBe(VOLUMES_REQUIREMENT)
  })
})

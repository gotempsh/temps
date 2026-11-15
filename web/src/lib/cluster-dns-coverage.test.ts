// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  ClusterDnsStatusResponse,
  DeploymentWithoutResolver,
} from '@/api/client'
import {
  hostsLabel,
  redeployWithoutResolver,
  withoutResolverForProject,
  type CoverageApi,
} from './cluster-dns-coverage'

function entry(
  overrides: Partial<DeploymentWithoutResolver> = {}
): DeploymentWithoutResolver {
  return {
    deployment_id: 41,
    project_id: 3,
    project_name: 'Shop',
    project_slug: 'shop',
    environment_id: 9,
    environment_name: 'production',
    hosts: [{ node_id: null, node_name: 'control plane' }],
    ...overrides,
  }
}

function status(
  overrides: Partial<ClusterDnsStatusResponse> = {}
): ClusterDnsStatusResponse {
  return {
    cluster_dns_enabled: true,
    total_record_count: 4,
    nodes: [],
    name_conflicts: [],
    deployments_without_resolver: [entry(), entry({ project_id: 5 })],
    deployments_without_resolver_error: null,
    ...overrides,
  }
}

describe('withoutResolverForProject', () => {
  test("keeps only the project's applications", () => {
    expect(withoutResolverForProject(status(), 3)).toEqual([entry()])
  })

  test('lists nothing while cluster DNS is off', () => {
    expect(
      withoutResolverForProject(status({ cluster_dns_enabled: false }), 3)
    ).toEqual([])
    expect(withoutResolverForProject(undefined, 3)).toEqual([])
  })
})

describe('hostsLabel', () => {
  test('names the control plane and workers', () => {
    expect(hostsLabel([{ node_id: null, node_name: 'control plane' }])).toBe(
      'the control plane'
    )
    expect(
      hostsLabel([
        { node_id: null, node_name: 'control plane' },
        { node_id: 1, node_name: 'worker-a' },
        { node_id: 2, node_name: 'worker-b' },
      ])
    ).toBe('the control plane, worker-a and worker-b')
  })
})

describe('redeployWithoutResolver', () => {
  test('rebuilds the current deployment from its own commit', async () => {
    const pipelines: unknown[] = []
    const api = {
      getProject: async () => ({ data: { source_type: 'git' } }),
      getDeployment: async () => ({
        data: {
          id: 41,
          environment_id: 9,
          branch: 'main',
          commit_hash: 'abc123',
          tag: null,
          metadata: null,
        },
      }),
      triggerProjectPipeline: async (options: unknown) => {
        pipelines.push(options)
        return { data: {} }
      },
      deployFromImage: async () => ({ data: {} }),
      deployFromStatic: async () => ({ data: {} }),
    } as unknown as CoverageApi

    const skipped = await redeployWithoutResolver(entry(), api)

    expect(skipped).toBeUndefined()
    expect(pipelines).toEqual([
      {
        path: { id: 3 },
        body: {
          branch: 'main',
          commit: 'abc123',
          tag: null,
          environment_id: 9,
        },
        throwOnError: true,
      },
    ])
  })
})

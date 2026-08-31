// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'

const BEARER_SECURITY = [{ scheme: 'bearer', type: 'http' }] as const

export interface ComposeSourceService {
  name: string
  image?: string | null
  looks_like_database: boolean
  detected_service_type?: 'postgres' | 'mariadb' | 'mongodb' | 'redis' | 's3'
  ports: Array<{
    target: number
    published?: number
    protocol: string
  }>
  health_check_path?: string | null
}

export interface ComposeSource {
  content: string
  revision: number
  checksum: string
  updated_at: string
  services: ComposeSourceService[]
  origin?: {
    provider: string
    slug: string
    sourceUrl: string
    sourceRevision?: string
    templateLastUpdatedAt?: string
  } | null
}

export interface ComposeDeployment {
  id: number
  project_id: number
  environment_id: number
  slug: string
  state: string
  source_type: string
  created_at: string
}

export async function getComposeSource(
  projectId: number,
  signal?: AbortSignal
): Promise<ComposeSource> {
  const { data } = await client.get<ComposeSource, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/projects/{project_id}/compose-source',
    path: { project_id: projectId },
    signal,
    throwOnError: true,
  })
  return data
}

export async function saveComposeSource(
  projectId: number,
  content: string,
  expectedRevision: number | null
): Promise<ComposeSource> {
  const { data } = await client.put<ComposeSource, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/projects/{project_id}/compose-source',
    path: { project_id: projectId },
    body: {
      content,
      expected_revision: expectedRevision,
    },
    headers: { 'Content-Type': 'application/json' },
    throwOnError: true,
  })
  return data
}

export async function deployComposeSource(
  projectId: number,
  environmentId: number,
  revision?: number
): Promise<ComposeDeployment> {
  const { data } = await client.post<ComposeDeployment, unknown, true>({
    security: [...BEARER_SECURITY],
    url: '/projects/{project_id}/environments/{environment_id}/deploy/compose',
    path: { project_id: projectId, environment_id: environmentId },
    body: { revision },
    headers: { 'Content-Type': 'application/json' },
    throwOnError: true,
  })
  return data
}

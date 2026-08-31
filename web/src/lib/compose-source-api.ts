// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deployComposeSource as deployComposeSourceRequest,
  getComposeSource as getComposeSourceRequest,
  saveComposeSource as saveComposeSourceRequest,
} from '@/api/client/sdk.gen'
import type {
  ComposeSourceResponse,
  ComposeSourceServiceResponse,
  RemoteDeploymentResponse,
} from '@/api/client/types.gen'

export type ComposeSourceService = ComposeSourceServiceResponse
export type ComposeSource = ComposeSourceResponse
export type ComposeDeployment = RemoteDeploymentResponse

export async function getComposeSource(
  projectId: number,
  signal?: AbortSignal
): Promise<ComposeSource> {
  const { data } = await getComposeSourceRequest({
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
  const { data } = await saveComposeSourceRequest({
    path: { project_id: projectId },
    body: {
      content,
      expected_revision: expectedRevision,
    },
    throwOnError: true,
  })
  return data
}

export async function deployComposeSource(
  projectId: number,
  environmentId: number,
  revision?: number
): Promise<ComposeDeployment> {
  const { data } = await deployComposeSourceRequest({
    path: { project_id: projectId, environment_id: environmentId },
    body: { revision },
    throwOnError: true,
  })
  return data
}

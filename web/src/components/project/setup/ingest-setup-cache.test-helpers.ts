// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Test helpers: a QueryClient whose `ingest-setup` query is already loaded,
 * failed, or failed after loading, so components render that state
 * synchronously without a network. Imported by tests only.
 */
import { QueryClient } from '@tanstack/react-query'
import type { ProjectIngestSetup } from '@/api/client'
import { getProjectIngestSetupOptions } from '@/api/client/@tanstack/react-query.gen'

export type IngestSetupCacheState =
  | { kind: 'loaded'; data: ProjectIngestSetup }
  | { kind: 'failed' }
  | { kind: 'failed-after-load'; data: ProjectIngestSetup }

export const FAILURE_DETAIL = 'Project 42 ingest setup failed'

export function ingestSetupClient(
  state: IngestSetupCacheState,
  projectId = 42,
  environmentId?: number
): QueryClient {
  // retryOnMount off: a failed query without data would otherwise render as
  // pending while it refetches on mount; this is the state after that
  // refetch failed too.
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, retryOnMount: false } },
  })
  const options = getProjectIngestSetupOptions({
    path: { project_id: projectId },
    query: environmentId ? { environment_id: environmentId } : undefined,
  })
  const query = client.getQueryCache().build(client, {
    queryKey: options.queryKey,
  })
  const now = Date.now()
  if (state.kind === 'loaded') {
    query.setState({ status: 'success', data: state.data, dataUpdatedAt: now })
  } else {
    query.setState({
      status: 'error',
      data: state.kind === 'failed-after-load' ? state.data : undefined,
      dataUpdatedAt: state.kind === 'failed-after-load' ? now : 0,
      error: { detail: FAILURE_DETAIL } as never,
      errorUpdatedAt: now,
      fetchStatus: 'idle',
    })
  }
  return client
}

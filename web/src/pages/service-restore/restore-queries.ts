// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getRestoreCapabilitiesQueryKey,
  getRestoreRunQueryKey,
  getServiceQueryKey,
  listRestoreRunsForServiceQueryKey,
  listS3SourcesQueryKey,
  listSourceBackupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import {
  getRestoreCapabilities,
  getRestoreRun,
  getService,
  listRestoreRunsForService,
  listS3Sources,
  listSourceBackups,
} from '@/api/client/sdk.gen'
import { queryOptions } from '@tanstack/react-query'
import { shouldRetryRead, toQueryError } from './restore-state'

// Query options for the restore page. They reuse the generated query keys so
// the cache is shared with the rest of the console, but read the HTTP status
// of a failure: the generated options throw only the Problem body, which
// carries no status, and the page has to tell "no access" from "not found"
// from "API down".

interface ReadResult<T> {
  data?: T
  error?: unknown
  response?: Response
}

async function requireData<T>(read: Promise<ReadResult<T>>): Promise<T> {
  const { data, error, response } = await read
  if (error !== undefined || data === undefined) {
    throw toQueryError(error, response?.status)
  }
  return data
}

export function restoreServiceQuery(serviceId: number) {
  const options = { path: { id: serviceId } }
  return queryOptions({
    queryKey: getServiceQueryKey(options),
    queryFn: ({ signal }) =>
      requireData(getService({ ...options, signal, throwOnError: false })),
    retry: (count, error) => shouldRetryRead(count, error),
  })
}

export function restoreCapabilitiesQuery(serviceId: number) {
  const options = { path: { id: serviceId } }
  return queryOptions({
    queryKey: getRestoreCapabilitiesQueryKey(options),
    queryFn: ({ signal }) =>
      requireData(
        getRestoreCapabilities({ ...options, signal, throwOnError: false })
      ),
    retry: (count, error) => shouldRetryRead(count, error),
  })
}

export function restoreSourcesQuery() {
  return queryOptions({
    queryKey: listS3SourcesQueryKey(),
    queryFn: ({ signal }) =>
      requireData(listS3Sources({ signal, throwOnError: false })),
    retry: (count, error) => shouldRetryRead(count, error),
  })
}

export function restoreSourceBackupsQuery(sourceId: number) {
  const options = { path: { id: sourceId } }
  return queryOptions({
    queryKey: listSourceBackupsQueryKey(options),
    queryFn: ({ signal }) =>
      requireData(
        listSourceBackups({ ...options, signal, throwOnError: false })
      ),
    retry: (count, error) => shouldRetryRead(count, error),
  })
}

/** Restore runs for a service, newest first: used to reattach to an active one. */
export function serviceRestoreRunsQuery(serviceId: number) {
  const options = { path: { id: serviceId } }
  return queryOptions({
    queryKey: listRestoreRunsForServiceQueryKey(options),
    queryFn: ({ signal }) =>
      requireData(
        listRestoreRunsForService({ ...options, signal, throwOnError: false })
      ),
    retry: (count, error) => shouldRetryRead(count, error),
  })
}

/**
 * One restore run's status. Retries an outage once quickly; the poll interval
 * (see `runPollInterval`) keeps trying after that, so a transient blip shows
 * as stale status rather than an error that never clears.
 */
export function restoreRunQuery(runId: number) {
  const options = { path: { id: runId } }
  return queryOptions({
    queryKey: getRestoreRunQueryKey(options),
    queryFn: ({ signal }) =>
      requireData(getRestoreRun({ ...options, signal, throwOnError: false })),
    retry: (count, error) => shouldRetryRead(count, error, 1),
    retryDelay: 1000,
  })
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Callout } from '@temps-sdk/ds'
import type { LogCollectionCapability } from '@/api/client/types.gen'

function formatBytes(bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`
}

function when(iso: string | null | undefined): string | undefined {
  return iso ? new Date(iso).toLocaleString() : undefined
}

/**
 * Why new log lines might not be arriving. Without it a paused collector
 * looks exactly like a quiet fleet: the histogram simply stops. Renders
 * nothing while collection is running and nothing was set aside.
 */
export function LogCollectionNotice({
  collection,
}: {
  collection: LogCollectionCapability | undefined
}) {
  if (!collection) return null
  return (
    <div className="space-y-3">
      <CollectionStateNotice collection={collection} />
      <DeferredGenerationsNotice collection={collection} />
    </div>
  )
}

function CollectionStateNotice({
  collection,
}: {
  collection: LogCollectionCapability
}) {
  switch (collection.state) {
    case 'running':
      return null
    case 'recovering': {
      const since = when(collection.since)
      return (
        <Callout
          tone="info"
          title="Catching up on logs from before the restart"
        >
          Temps is replaying log lines it had buffered when it last stopped
          {since ? ` (started ${since})` : ''}. New lines are collected as soon
          as that finishes.
        </Callout>
      )
    }
    case 'retrying': {
      const retryAt = when(collection.retry_at)
      return (
        <Callout tone="error" title="Log collection is paused">
          <p>
            Replaying buffered log lines after the last restart failed, so new
            lines are not being collected. Temps retries on its own
            {retryAt ? ` at ${retryAt}` : ''}.
          </p>
          <RecoveryError collection={collection} />
        </Callout>
      )
    }
    case 'stopped':
      return (
        <Callout tone="error" title="Log collection is stopped">
          <p>
            Replaying buffered log lines after the last restart failed in a way
            retrying cannot fix, so new lines are not being collected. Fix the
            error below, then restart temps.
          </p>
          <RecoveryError collection={collection} />
        </Callout>
      )
  }
}

const ADMIN_ONLY =
  'An instance administrator can see the exact error and file locations here.'

/** The raw error carries server paths, so the API sends it to admins only. */
function RecoveryError({
  collection,
}: {
  collection: LogCollectionCapability
}) {
  if (!collection.details_visible) {
    return <p className="mt-1 text-xs">{ADMIN_ONLY}</p>
  }
  if (!collection.error) return null
  return (
    <p className="mt-1 break-words font-mono text-xs">{collection.error}</p>
  )
}

function DeferredGenerationsNotice({
  collection,
}: {
  collection: LogCollectionCapability
}) {
  if (collection.deferred_count === 0) return null
  const hidden = collection.deferred_count - collection.deferred.length
  return (
    <Callout
      tone="warning"
      title={`${collection.deferred_count.toLocaleString()} buffered log ${
        collection.deferred_count === 1 ? 'file' : 'files'
      } could not be replayed`}
    >
      <p>
        Collection continues, but the lines in{' '}
        {collection.deferred_count === 1 ? 'this file' : 'these files'} (
        {formatBytes(collection.deferred_bytes)}) are missing from search.
        {collection.details_visible ? (
          <>
            {' '}
            They were moved to{' '}
            <code className="font-mono text-xs">
              {collection.deferred_dir ?? 'the deferred WAL directory'}
            </code>
            , each beside a <code className="font-mono text-xs">.reason</code>{' '}
            file saying what could not be read. Temps never replays them on its
            own; to retry one, move it back into the WAL directory and restart
            temps.
          </>
        ) : (
          ` ${ADMIN_ONLY}`
        )}
      </p>
      <details className="mt-2">
        <summary className="cursor-pointer text-xs">Show files</summary>
        <ul className="mt-1 space-y-1 text-xs">
          {collection.deferred.map((generation) => (
            <li key={generation.file_name} className="break-words">
              <span className="font-mono">{generation.file_name}</span>
              {' · '}
              {formatBytes(generation.bytes)}
              {generation.reason ? ` · ${generation.reason}` : ''}
            </li>
          ))}
          {hidden > 0 ? <li>…and {hidden.toLocaleString()} more</li> : null}
        </ul>
      </details>
    </Callout>
  )
}

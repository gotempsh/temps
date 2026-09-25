// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { LogCollectionCapability } from '@/api/client/types.gen'
import { LogCollectionNotice } from './LogCollectionNotice'

const running: LogCollectionCapability = {
  state: 'running',
  collecting: true,
  deferred_count: 0,
  deferred_bytes: 0,
  deferred: [],
  details_visible: true,
}

const render = (collection: LogCollectionCapability | undefined) =>
  renderToStaticMarkup(<LogCollectionNotice collection={collection} />)

test('says nothing while logs are being collected', () => {
  expect(render(running)).toBe('<div class="space-y-3"></div>')
  expect(render(undefined)).toBe('')
})

test('a paused collector explains itself instead of letting the histogram just stop', () => {
  const markup = render({
    ...running,
    state: 'retrying',
    collecting: false,
    error: 'object storage unreachable',
    retry_at: '2026-01-01T00:10:00Z',
  })
  expect(markup).toContain('Log collection is paused')
  expect(markup).toContain('object storage unreachable')
  expect(markup).toContain('retries on its own')
})

test('a stopped collector says a restart is needed', () => {
  const markup = render({
    ...running,
    state: 'stopped',
    collecting: false,
    error: 'permission denied',
  })
  expect(markup).toContain('Log collection is stopped')
  expect(markup).toContain('restart temps')
})

test('deferred files are surfaced with where they are and how to retry', () => {
  const markup = render({
    ...running,
    deferred_count: 3,
    deferred_bytes: 3 * 1024 * 1024,
    deferred_dir: '/data/logs/wal/deferred',
    deferred: [
      {
        file_name: 'a.b.sealed-wal',
        bytes: 1024,
        reason: 'record at byte 42 fails its checksum',
      },
    ],
  })
  expect(markup).toContain('3 buffered log files could not be replayed')
  expect(markup).toContain('3.0 MiB')
  expect(markup).toContain('/data/logs/wal/deferred')
  expect(markup).toContain('record at byte 42 fails its checksum')
  expect(markup).toContain('…and 2 more')
})

test('non-administrators are told details exist rather than shown an empty error', () => {
  const markup = render({
    ...running,
    state: 'stopped',
    collecting: false,
    details_visible: false,
    deferred_count: 2,
    deferred_bytes: 2048,
    deferred: [{ file_name: 'a.b.sealed-wal', bytes: 1024 }],
  })
  expect(markup).toContain('Log collection is stopped')
  expect(markup).toContain('An instance administrator can see the exact error')
  expect(markup).not.toContain('.reason')
})

test('a failed status request is shown instead of implying collection is fine', () => {
  const markup = renderToStaticMarkup(
    <LogCollectionNotice
      collection={undefined}
      statusError={new Error('Request failed with status 502')}
      onRetry={() => {}}
    />
  )
  expect(markup).toContain('Log collection status is unavailable')
  expect(markup).toContain('Request failed with status 502')
  expect(markup).toContain('Try again')
})

test('an unreadable deferred directory is reported alongside the running state', () => {
  const markup = render({
    ...running,
    deferred_error: 'IO error: Permission denied',
  })
  expect(markup).toContain(
    'Could not check for log files set aside by recovery'
  )
  expect(markup).toContain('Permission denied')
})

test('a failed refresh is shown even when an earlier status is still cached', () => {
  const markup = renderToStaticMarkup(
    <LogCollectionNotice
      collection={running}
      statusError={new Error('Request failed with status 502')}
      statusUpdatedAt={Date.UTC(2026, 0, 1)}
      onRetry={() => {}}
    />
  )
  expect(markup).toContain('Log collection status is unavailable')
  expect(markup).toContain('Request failed with status 502')
  expect(markup).toContain('may be out of date')
  expect(markup).toContain('Try again')
})

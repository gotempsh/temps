// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type {
  ArchiveTargetTestResponse,
  NotificationDeliveryCoverageResponse,
  PostgresWalHealth,
} from '@/api/client/types.gen'
import { archivingState } from '@/lib/wal-health'
import {
  WalArchivingStatus,
  WalArchivingStatusView,
} from './WalArchivingStatus'

const snapshot: PostgresWalHealth = {
  probed_at: '2026-01-01T02:00:00Z',
  archive_mode: 'on',
  archive_command: 'wal-g wal-push %p',
  archive_backlog: 0,
  pg_wal_bytes: 64 * 1024 * 1024,
  max_wal_size_bytes: 1024 * 1024 * 1024,
  oldest_wal_age_secs: 0,
  stale_slots: [],
  warnings: [],
}

function render(props: {
  snapshot?: PostgresWalHealth
  coverage?: NotificationDeliveryCoverageResponse
  result?: ArchiveTargetTestResponse
  testError?: { message: string; setupPath?: string }
}): string {
  return renderToStaticMarkup(
    <MemoryRouter>
      <WalArchivingStatusView
        snapshot={props.snapshot ?? snapshot}
        coverage={props.coverage}
        returnTo="/storage/7"
        testing={false}
        result={props.result}
        testError={props.testError}
        onTest={() => {}}
      />
    </MemoryRouter>
  )
}

describe('archivingState', () => {
  test('a failure after the last success is failing', () => {
    expect(
      archivingState({
        ...snapshot,
        archiver_last_archived_at: '2026-01-01T00:00:00Z',
        archiver_last_failed_at: '2026-01-01T01:00:00Z',
      })
    ).toBe('failing')
  })

  test('a no-op archive_command is not archiving, whatever the counters say', () => {
    const noop: PostgresWalHealth = {
      ...snapshot,
      archive_command: '/bin/true',
      archiver_last_archived_at: '2026-01-01T01:00:00Z',
      warnings: [{ kind: 'archive_mode_without_command' }],
    }
    expect(archivingState(noop)).toBe('discarding')
    const html = render({ snapshot: noop })
    expect(html).toContain('Not storing WAL: archive_command does nothing')
    expect(html).not.toContain('>Archiving<')
  })

  test('an empty archive_command keeps WAL rather than discarding it', () => {
    const empty: PostgresWalHealth = {
      ...snapshot,
      archive_command: null,
      warnings: [{ kind: 'archive_mode_without_command' }],
    }
    expect(archivingState(empty)).toBe('no_command')
    const html = render({ snapshot: empty })
    expect(html).toContain('archive_command is empty, so pg_wal keeps growing')
    expect(html).not.toContain('Not storing WAL')
  })

  test('archiving through archive_library is not flagged as a missing command', () => {
    expect(
      archivingState({
        ...snapshot,
        archive_command: null,
        archive_library: 'basic_archive',
        archiver_last_archived_at: '2026-01-01T01:00:00Z',
      })
    ).toBe('archiving')
  })

  test('a success after the last failure is archiving again', () => {
    expect(
      archivingState({
        ...snapshot,
        archiver_last_archived_at: '2026-01-01T01:00:00Z',
        archiver_last_failed_at: '2026-01-01T00:00:00Z',
      })
    ).toBe('archiving')
  })

  test('a failure and never a success is failing', () => {
    expect(
      archivingState({
        ...snapshot,
        archiver_last_failed_at: '2026-01-01T00:00:00Z',
      })
    ).toBe('failing')
  })

  test('a segment waiting five minutes with no failure is failing', () => {
    expect(
      archivingState({
        ...snapshot,
        archiver_last_archived_at: '2026-01-01T00:00:00Z',
        archive_backlog: 5,
        archive_oldest_ready_age_secs: 600,
      })
    ).toBe('failing')
    expect(
      archivingState({
        ...snapshot,
        archiver_last_archived_at: '2026-01-01T00:00:00Z',
        archive_backlog: 1,
        archive_oldest_ready_age_secs: 3,
      })
    ).toBe('archiving')
  })

  test('nothing archived and nothing failed is waiting', () => {
    expect(archivingState(snapshot)).toBe('waiting')
  })
})

describe('WalArchivingStatus', () => {
  test('is keyed by service, so a check never outlives the page it ran on', () => {
    const element = WalArchivingStatus({ serviceId: 7, snapshot })
    expect(element.key).toBe('7')
    expect(WalArchivingStatus({ serviceId: 8, snapshot }).key).toBe('8')
  })
})

describe('WalArchivingStatusView', () => {
  test('shows the last success, failure, segment and pg_wal size', () => {
    const html = render({
      snapshot: {
        ...snapshot,
        archiver_last_archived_at: '2026-01-01T00:00:00Z',
        archiver_last_failed_at: '2026-01-01T01:00:00Z',
        archiver_last_failed_wal: '000000010000000000000042',
      },
    })
    expect(html).toContain('Failing')
    expect(html).toContain('000000010000000000000042')
    expect(html).toContain('64 MB of 1.0 GB max_wal_size')
    expect(html).toContain('Test archive target')
  })

  test('a refused check names the endpoint, scheme, path style and cause', () => {
    const html = render({
      result: {
        ok: false,
        s3_source_id: 3,
        s3_source_name: 'archive',
        endpoint: 'http://10.0.0.5:9000',
        scheme: 'http',
        force_path_style: true,
        path_style_reason: 'forced: the endpoint is an IP address',
        message: 'connection refused',
        checked_at: '2026-01-01T02:00:00Z',
      },
    })
    expect(html).toContain('WAL-G could not write to the archive target')
    expect(html).toContain('http://10.0.0.5:9000')
    expect(html).toContain('scheme http')
    expect(html).toContain('path-style addressing on')
    expect(html).toContain('forced: the endpoint is an IP address')
    expect(html).toContain('connection refused')
  })

  test('settings that work but are not the ones in use say how to apply them', () => {
    const html = render({
      result: {
        ok: true,
        s3_source_id: 3,
        s3_source_name: 'archive',
        endpoint: 'http://10.0.0.5:9000',
        scheme: 'http',
        force_path_style: true,
        path_style_reason: 'forced: the endpoint is an IP address',
        message: 'WAL-G wrote a probe object to the archive target',
        live_settings_drift:
          "The running container's walg.env sends WAL to 10.0.0.5:9000 (no scheme, so WAL-G uses https) instead of http://10.0.0.5:9000. Restart the service.",
        checked_at: '2026-01-01T02:00:00Z',
      },
    })
    expect(html).toContain('WAL-G can write to the archive target')
    expect(html).toContain(
      'The running database archives with different settings'
    )
    expect(html).toContain('Restart the service')
  })

  test('a check that could not run links to the fix', () => {
    const html = render({
      testError: {
        message: 'run a backup of it to turn continuous archiving on',
        setupPath: '#backups',
      },
    })
    expect(html).toContain('The archive target could not be tested')
    expect(html).toContain('#backups')
  })

  test('without notifications it says the alarms reach no one and how to fix it', () => {
    const html = render({
      coverage: {
        configured: false,
        reason: 'No notification provider is configured',
        setup_path: '/settings/notifications/new',
        severity: 'critical',
        provider_ids: [],
        route_ids: [],
        cloud_delivery: false,
      },
    })
    expect(html).toContain('Archiving alarms won&#x27;t notify anyone yet')
    expect(html).toContain('No notification provider is configured')
    expect(html).toContain('Add a notification provider')
    expect(html).toContain('/settings/notifications/new')
  })

  test('with notifications configured there is no onboarding notice', () => {
    const html = render({
      coverage: {
        configured: true,
        severity: 'critical',
        provider_ids: [1],
        route_ids: [1],
        cloud_delivery: false,
      },
    })
    expect(html).not.toContain('won&#x27;t notify')
  })
})

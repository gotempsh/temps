// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { OperationEntry } from '@/api/client/types.gen'
import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import {
  OPERATIONS_EMPTY_MESSAGE,
  OperationsTrayPanel,
  type OperationsTrayPanelProps,
} from './OperationsTrayPanel'

const NOW = Date.parse('2026-10-07T12:00:00Z')
const noop = () => {}

function entry(overrides: Partial<OperationEntry> = {}): OperationEntry {
  return {
    id: 'deployment:42',
    kind: 'rollback',
    status: 'running',
    title: 'Rollback to deployment #41',
    project_id: 3,
    project_slug: 'shop',
    environment_id: 5,
    environment_name: 'production',
    deployment_id: 42,
    service_id: null,
    service_name: null,
    backup_id: null,
    restore_run_id: null,
    agent_run_id: null,
    phase: null,
    failure_reason: null,
    created_at: '2026-10-07T11:55:00Z',
    started_at: '2026-10-07T11:56:00Z',
    finished_at: null,
    triggered_by_user_id: null,
    link: '/projects/shop/deployments/42',
    ...overrides,
  }
}

function render(props: Partial<OperationsTrayPanelProps> = {}) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <OperationsTrayPanel
        operations={[]}
        localOperations={[]}
        runningCount={0}
        isPending={false}
        isError={false}
        onRetry={noop}
        onNavigate={noop}
        now={NOW}
        {...props}
      />
    </MemoryRouter>
  )
}

describe('OperationsTrayPanel', () => {
  test('loading renders skeleton rows, not a spinner', () => {
    const html = render({ isPending: true })
    expect(html).toContain('animate-pulse')
    expect(html).not.toContain('animate-spin')
  })

  test('error state explains and offers retry', () => {
    const html = render({ isError: true, errorMessage: 'Database timeout' })
    expect(html).toContain('load operations')
    expect(html).toContain('Database timeout')
    expect(html).toContain('Retry')
  })

  test('empty state says what will appear', () => {
    expect(render()).toContain(OPERATIONS_EMPTY_MESSAGE)
  })

  test('rows link to the resource with status, context and time', () => {
    const html = render({ operations: [entry()], runningCount: 1 })
    expect(html).toContain('href="/projects/shop/deployments/42"')
    expect(html).toContain('Rollback to deployment #41')
    expect(html).toContain('Running')
    expect(html).toContain('shop · production')
    expect(html).toContain('4m ago')
    expect(html).toContain('1 running')
  })

  test('failed rows show the failure reason', () => {
    const html = render({
      operations: [
        entry({
          id: 'backup:3',
          kind: 'backup',
          status: 'failed',
          title: 'Backup of orders-db',
          failure_reason: 'bucket unreachable',
          finished_at: '2026-10-07T11:00:00Z',
          link: '/backups/s3-sources/2/backups/b-uuid',
        }),
      ],
    })
    expect(html).toContain('Failed')
    expect(html).toContain('bucket unreachable')
    expect(html).toContain('1h ago')
  })

  test('groups in-progress and recent work when both exist', () => {
    const html = render({
      operations: [
        entry(),
        entry({ id: 'deployment:40', status: 'succeeded', title: 'Deploy' }),
      ],
      runningCount: 1,
    })
    expect(html).toContain('In progress')
    expect(html).toContain('Recent')
  })

  test('local entries are listed first and marked as not persisted', () => {
    const html = render({
      localOperations: [
        {
          id: 'local:1',
          title: 'Restarting container',
          context: 'abc123def456',
          startedAt: NOW,
        },
      ],
    })
    expect(html).toContain('Restarting container')
    expect(html).toContain('not kept after a refresh')
    expect(html).not.toContain(OPERATIONS_EMPTY_MESSAGE)
    expect(html).toContain('1 running')
  })
})

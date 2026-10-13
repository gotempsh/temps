// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { AlarmResponse } from '@/api/client'
import { alarmQuickAction, alarmScopeLinks } from './alarm-actions'

function alarm(overrides: Partial<AlarmResponse>): AlarmResponse {
  return {
    id: 1,
    alarm_type: 'high_cpu',
    created_at: '2026-10-06T12:00:00Z',
    fired_at: '2026-10-06T12:00:00Z',
    severity: 'warning',
    status: 'firing',
    title: 'Alarm',
    updated_at: '2026-10-06T12:00:00Z',
    project_id: 1,
    ...overrides,
  }
}

const containerAlarm = alarm({
  alarm_type: 'container_restart',
  environment_id: 4,
  deployment_id: 30,
  container_id: 9,
  metadata: { container_id: 'f00dcafe', container_name: 'web-app-1' },
})

describe('alarmScopeLinks', () => {
  test('links every scope part to its page', () => {
    expect(alarmScopeLinks(containerAlarm, 'app')).toEqual([
      { label: 'env #4', href: '/projects/app/environments?environment=4' },
      { label: 'deploy #30', href: '/projects/app/deployments/30' },
      {
        label: 'web-app-1',
        href: '/projects/app/environments/containers/f00dcafe?env=4',
      },
    ])
  })

  test('links a service scope to the service page', () => {
    expect(alarmScopeLinks(alarm({ service_id: 12 }), 'app')).toEqual([
      { label: 'service #12', href: '/storage/12' },
    ])
  })

  test('keeps the container label when its Docker ID is unknown', () => {
    const links = alarmScopeLinks(
      alarm({ environment_id: 4, container_id: 9 }),
      'app'
    )
    expect(links[1]).toEqual({ label: 'container #9', href: undefined })
  })

  test('links an error-tracking alarm to its error group', () => {
    const links = alarmScopeLinks(
      alarm({
        alarm_type: 'error_tracking_threshold',
        metadata: { group_id: 77 },
      }),
      'app'
    )
    expect(links).toEqual([
      { label: 'error group #77', href: '/projects/app/errors/77' },
    ])
  })

  test('reads project-wide when the alarm has no scope', () => {
    expect(alarmScopeLinks(alarm({}), 'app')).toEqual([
      { label: 'project-wide' },
    ])
  })

  test('leaves project pages unlinked until the project is known', () => {
    expect(alarmScopeLinks(alarm({ deployment_id: 3 }), undefined)).toEqual([
      { label: 'deploy #3', href: undefined },
    ])
  })
})

describe('alarmQuickAction', () => {
  test('offers a restart for a crashing container', () => {
    expect(alarmQuickAction(containerAlarm, 'app')).toEqual({
      kind: 'restart_container',
      environmentId: 4,
      containerId: 'f00dcafe',
      containerName: 'web-app-1',
    })
    expect(
      alarmQuickAction(
        { ...containerAlarm, alarm_type: 'container_oom_killed' },
        'app'
      )?.kind
    ).toBe('restart_container')
  })

  test('offers no restart without the Docker container ID', () => {
    expect(alarmQuickAction({ ...containerAlarm, metadata: null }, 'app')).toBe(
      null
    )
  })

  test('offers a redeploy for a failed deployment', () => {
    expect(
      alarmQuickAction(
        alarm({ alarm_type: 'deployment_failed', deployment_id: 30 }),
        'app'
      )
    ).toEqual({ kind: 'redeploy', deploymentId: 30 })
  })

  test('offers autofix for an error-tracking alarm', () => {
    expect(
      alarmQuickAction(
        alarm({
          alarm_type: 'error_tracking_threshold',
          metadata: { group_id: 77 },
        }),
        'app'
      )
    ).toEqual({ kind: 'autofix', href: '/projects/app/errors/77/autofix' })
  })

  test('offers nothing for resolved alarms or types without a fix', () => {
    expect(
      alarmQuickAction({ ...containerAlarm, status: 'resolved' }, 'app')
    ).toBe(null)
    expect(alarmQuickAction(alarm({ alarm_type: 'high_cpu' }), 'app')).toBe(
      null
    )
  })
})

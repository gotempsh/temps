// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { serviceFailureSummary, type ServiceDownAlert } from './service-health'

const alarm = (
  overrides: Partial<ServiceDownAlert> = {}
): ServiceDownAlert => ({
  alarm_id: 11,
  alarm_status: 'firing',
  alarm_fired_at: '2026-01-01T10:00:00Z',
  alert_severity: 'critical',
  notification_destinations: 2,
  notification_setup_path: '/settings/notifications/new',
  ...overrides,
})

const summarize = (failures: number, down_alert?: ServiceDownAlert) =>
  serviceFailureSummary(
    { consecutive_failures: failures, down_alert },
    7,
    (iso) => `<${iso}>`
  )

describe('service failure summary', () => {
  it('never claims this alarm was sent, delivered or routed', () => {
    const cases = [
      summarize(4),
      summarize(4, alarm()),
      summarize(4, alarm({ notification_destinations: 1 })),
      summarize(4, alarm({ notification_destinations: 0 })),
      summarize(4, alarm({ silenced_until: '2026-01-01T12:00:00Z' })),
      summarize(4, alarm({ alarm_id: null })),
      summarize(4, alarm({ notification_destinations: null })),
    ]
    for (const summary of cases) {
      const text = `${summary.headline} ${summary.alertNote ?? ''}`
      expect(text).not.toContain('an alert has been sent')
      expect(text).not.toMatch(/\b(was|were|been) (sent|delivered|routed)\b/)
      expect(text).not.toMatch(/routed to your/)
    }
  })

  it('stays below the threshold without mentioning alarms', () => {
    expect(summarize(2)).toEqual({
      headline: 'Service has failed 2 check(s) in a row.',
    })
  })

  it('explains that nobody receives alerts of the alarm severity and links to setup', () => {
    const summary = summarize(4, alarm({ notification_destinations: 0 }))
    expect(summary.headline).toBe('Service has failed 4 consecutive checks.')
    expect(summary.alertNote).toContain(
      'no notification destination receives critical alerts'
    )
    expect(summary.setupHref).toBe(
      '/settings/notifications/new?returnTo=%2Fstorage%2F7'
    )
  })

  it('reports a silenced alarm with its expiry', () => {
    const summary = summarize(
      5,
      alarm({ silenced_until: '2026-01-01T12:00:00Z' })
    )
    expect(summary.alertNote).toContain('silenced until <2026-01-01T12:00:00Z>')
    expect(summary.setupHref).toBeUndefined()
  })

  it('states current destinations for the severity without claiming delivery', () => {
    const many = summarize(3, alarm())
    expect(many.alertNote).toContain(
      '2 notification destinations currently receive critical alerts'
    )
    expect(many.alertNote).toContain(
      "Temps doesn't record whether this alarm reached them"
    )
    expect(many.alarmHref).toBe('/monitoring/alarms')
    expect(
      summarize(3, alarm({ notification_destinations: 1 })).alertNote
    ).toContain('1 notification destination currently receives critical')
  })

  it('does not guess when the destination count is unknown', () => {
    expect(
      summarize(3, alarm({ notification_destinations: null })).alertNote
    ).toBe('A down alarm was raised.')
  })

  it('distinguishes a detected failure with no alarm on record', () => {
    expect(summarize(3, alarm({ alarm_id: null })).alertNote).toBe(
      'No open down alarm is recorded for this service.'
    )
  })
})

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
  notifications_configured: true,
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
  it('never claims an alert was sent', () => {
    const cases = [
      summarize(4),
      summarize(4, alarm()),
      summarize(4, alarm({ notifications_configured: false })),
      summarize(4, alarm({ silenced_until: '2026-01-01T12:00:00Z' })),
      summarize(4, alarm({ alarm_id: null })),
      summarize(4, alarm({ notifications_configured: null })),
    ]
    for (const summary of cases) {
      const text = `${summary.headline} ${summary.alertNote ?? ''}`
      expect(text).not.toContain('an alert has been sent')
      expect(text).not.toMatch(/\b(was|were|been) (sent|delivered)\b/)
    }
  })

  it('stays below the threshold without mentioning alarms', () => {
    expect(summarize(2)).toEqual({
      headline: 'Service has failed 2 check(s) in a row.',
    })
  })

  it('explains a missing notification destination and links to setup', () => {
    const summary = summarize(4, alarm({ notifications_configured: false }))
    expect(summary.headline).toBe('Service has failed 4 consecutive checks.')
    expect(summary.alertNote).toContain(
      'no notification provider is configured'
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

  it('says the alarm was routed, not delivered, when providers exist', () => {
    const summary = summarize(3, alarm())
    expect(summary.alertNote).toContain('routed to your notification providers')
    expect(summary.alertNote).toContain("Delivery isn't confirmed")
    expect(summary.alarmHref).toBe('/monitoring/alarms')
  })

  it('distinguishes a detected failure with no alarm on record', () => {
    expect(summarize(3, alarm({ alarm_id: null })).alertNote).toBe(
      'No open down alarm is recorded for this service.'
    )
  })
})

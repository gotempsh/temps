// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { NotificationPreferencesResponse } from '@/api/client/types.gen'
import { notificationPreferencesUpdate } from './notification-preferences-payload'
import { notificationSettingsSchema } from './schemas'

const stored: NotificationPreferencesResponse = {
  backup_failures_enabled: true,
  backup_successes_enabled: false,
  batch_similar_notifications: false,
  build_errors_enabled: true,
  deployment_failures_enabled: true,
  digest_sections: {
    performance: true,
    deployments: true,
    errors: true,
    funnels: true,
    projects: true,
  },
  digest_send_day: 'monday',
  digest_send_time: '09:00',
  dns_changes_enabled: false,
  domain_expiration_enabled: true,
  email_enabled: true,
  error_threshold: 10,
  error_time_window: 5,
  load_balancer_issues_enabled: true,
  minimum_severity: 'warning',
  retention_policy_violations_enabled: false,
  route_downtime_enabled: true,
  runtime_errors_enabled: true,
  s3_connection_issues_enabled: true,
  slack_enabled: false,
  ssl_days_before_expiration: 30,
  ssl_expiration_enabled: true,
  weekly_digest_enabled: false,
}

test('saves exactly the stored preference fields, with no Slack webhook', () => {
  const body = notificationPreferencesUpdate(stored, {
    email: false,
    slack: { enabled: true },
    batchNotifications: true,
    minimumSeverity: 'critical',
  })

  expect(body).toEqual({
    preferences: {
      ...stored,
      email_enabled: false,
      slack_enabled: true,
      batch_similar_notifications: true,
      minimum_severity: 'critical',
    },
  })
  expect(JSON.stringify(body)).not.toContain('webhook')
})

test('the form no longer carries a Slack webhook that would be dropped', () => {
  const parsed = notificationSettingsSchema.parse({
    email: true,
    slack: { enabled: true, webhook: 'https://hooks.slack.com/services/x' },
    batchNotifications: false,
    minimumSeverity: 'warning',
  })
  expect(parsed.slack).toEqual({ enabled: true })
})

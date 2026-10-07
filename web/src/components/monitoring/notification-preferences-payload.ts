// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  NotificationPreferencesResponse,
  UpdatePreferencesRequest,
} from '@/api/client/types.gen'
import type { NotificationSettingsFormData } from './schemas'

/**
 * Request body for saving the Delivery preferences form. Only fields the
 * preferences API actually stores are written; Slack destinations live on a
 * Slack notification provider, not here.
 */
export function notificationPreferencesUpdate(
  preferences: NotificationPreferencesResponse,
  data: NotificationSettingsFormData
): UpdatePreferencesRequest {
  return {
    preferences: {
      ...preferences,
      email_enabled: data.email,
      slack_enabled: data.slack.enabled,
      batch_similar_notifications: data.batchNotifications,
      minimum_severity: data.minimumSeverity,
    },
  }
}

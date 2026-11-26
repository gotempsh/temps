// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps email-providers` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getEmailTrackingStatus,
  setupEmailTracking,
  updateEmailProvider,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerEmailProvidersEndpoints(program: Command): void {
  const group = topGroup(program, 'email-providers', "email-providers")

  endpoint(group, updateEmailProvider, {
    name: "update",
    description: "Update an email provider",
    path: { id: 'int' },
    body: {
      is_active: 'bool',
      name: 'string',
      region: 'string',
      scaleway_credentials: 'json',
      ses_credentials: 'json',
      smtp_credentials: { kind: 'json', help: "Generic SMTP credentials request body" },
      sns_topic_arn: { kind: 'string', help: "Rotate or clear the exact SNS topic allowed for this SES provider" },
    },
  })

  endpoint(group, setupEmailTracking, {
    name: "setup-tracking",
    description: "One-click AWS-side setup of SES event tracking (SNS topic + webhook subscription + SESv2 event..",
    path: { id: 'int' },
  })

  endpoint(group, getEmailTrackingStatus, {
    name: "get-tracking-status",
    description: "Live status of SES event tracking for a provider",
    path: { id: 'int' },
  })
}

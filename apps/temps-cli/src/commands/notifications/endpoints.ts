// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps notifications` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createCloudflareProvider,
  createNotificationEmailProvider,
  createWebhookProvider,
  getNotificationDeliveryCoverage,
  revealNotificationProviderConfig,
  testNotificationRoute,
  updateCloudflareProvider,
  updateWebhookProvider,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerNotificationsEndpoints(program: Command): void {
  const group = topGroup(program, 'notifications', "notifications")

  endpoint(group, createCloudflareProvider, {
    name: "create-cloudflare-provider",
    description: "Create a new Cloudflare Email Sending notification provider",
    body: {
      config: { kind: 'json!', help: "Configuration for a Cloudflare Email Sending notification provider" },
      enabled: 'bool',
      name: 'string!',
    },
  })

  endpoint(group, updateCloudflareProvider, {
    name: "update-cloudflare-provider",
    description: "Update a Cloudflare Email Sending notification provider",
    path: { id: 'int' },
    body: {
      config: { kind: 'json!', help: "Configuration for a Cloudflare Email Sending notification provider" },
      enabled: 'bool',
      name: 'string',
    },
  })

  endpoint(group, createNotificationEmailProvider, {
    name: "create-email-provider",
    description: "Create a new Email notification provider",
    body: {
      config: { kind: 'json!', help: "Request-side SMTP config" },
      enabled: 'bool',
      name: 'string!',
    },
  })

  endpoint(group, createWebhookProvider, {
    name: "create-webhook-provider",
    description: "Create a new Webhook notification provider",
    body: {
      config: { kind: 'json!', help: "Configuration for a generic webhook notification provider" },
      enabled: 'bool',
      name: 'string!',
    },
  })

  endpoint(group, updateWebhookProvider, {
    name: "update-webhook-provider",
    description: "Update a Webhook notification provider",
    path: { id: 'int' },
    body: {
      config: { kind: 'json!', help: "Configuration for a generic webhook notification provider" },
      enabled: 'bool',
      name: 'string',
    },
  })

  endpoint(group, revealNotificationProviderConfig, {
    name: "reveal-provider-config",
    description: "Reveal notification provider config",
    path: { id: 'int', field: 'string' },
  })

  endpoint(group, getNotificationDeliveryCoverage, {
    name: "routes get-delivery-coverage",
    description: "Check whether a notification of the given severity would be delivered",
    query: {
      severity: { kind: 'string!', help: "Severity of the notification to check: debug, info, warning, error, critical or emergency" },
    },
  })

  endpoint(group, testNotificationRoute, {
    name: "routes test",
    description: "Send a test notification through a route to each of its providers",
    path: { id: 'int' },
  })
}

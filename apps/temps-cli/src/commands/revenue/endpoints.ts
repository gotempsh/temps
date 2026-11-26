// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps revenue` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  revenueCreateIntegration,
  revenueDeleteIntegration,
  revenueGlobalEvents,
  revenueImportInvoicesCsv,
  revenueImportSubscriptionsCsv,
  revenueListIntegrations,
  revenueListProviders,
  revenueMetricsCustomers,
  revenueMetricsGlobalMrr,
  revenueMetricsGlobalSummary,
  revenueMetricsMrr,
  revenueMetricsSummary,
  revenueRecentEvents,
  revenueRotateToken,
  revenueUpdateConfig,
  revenueUpdateSecret,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerRevenueEndpoints(program: Command): void {
  const group = topGroup(program, 'revenue', "revenue")

  endpoint(group, revenueRecentEvents, {
    name: "recent-events",
    description: "Recent ingested events for the activity feed",
    path: { project_id: 'project' },
  })

  endpoint(group, revenueListIntegrations, {
    name: "list-integrations",
    description: "List revenue integrations for a project",
    path: { project_id: 'project' },
  })

  endpoint(group, revenueCreateIntegration, {
    name: "create-integration",
    description: "Create a new revenue integration",
    path: { project_id: 'project' },
    body: {
      provider: { kind: 'string!', help: "Registered provider name, e.g. \"stripe\"" },
      signing_secret: { kind: 'secret!', help: "Signing secret from the provider's dashboard" },
    },
  })

  endpoint(group, revenueDeleteIntegration, {
    name: "delete-integration",
    description: "Delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history)",
    path: { project_id: 'project', integration_id: 'int' },
    confirm: "This will delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history).",
  })

  endpoint(group, revenueUpdateConfig, {
    name: "update-config",
    description: "Replace the typed provider config on an integration",
    path: { project_id: 'project', integration_id: 'int' },
    body: {
      config: { kind: 'json', help: "Provider-specific integration settings persisted in 'revenue_integrations.config'" },
    },
  })

  endpoint(group, revenueImportInvoicesCsv, {
    name: "import-invoices-csv",
    description: "Import a Stripe invoices CSV export",
    path: { project_id: 'project', integration_id: 'int' },
  })

  endpoint(group, revenueImportSubscriptionsCsv, {
    name: "import-subscriptions-csv",
    description: "Import a Stripe subscriptions CSV export",
    path: { project_id: 'project', integration_id: 'int' },
  })

  endpoint(group, revenueRotateToken, {
    name: "rotate-token",
    description: "Rotate the webhook path token",
    path: { project_id: 'project', integration_id: 'int' },
  })

  endpoint(group, revenueUpdateSecret, {
    name: "update-secret",
    description: "Replace the stored signing secret without rotating the webhook URL",
    path: { project_id: 'project', integration_id: 'int' },
    body: {
      signing_secret: { kind: 'secret!', help: "New signing secret from the provider's dashboard" },
    },
  })

  endpoint(group, revenueMetricsCustomers, {
    name: "metrics-customers",
    description: "New + churned customers per bucket",
    path: { project_id: 'project' },
  })

  endpoint(group, revenueMetricsMrr, {
    name: "metrics-mrr",
    description: "Bucketed MRR timeseries for the revenue chart",
    path: { project_id: 'project' },
  })

  endpoint(group, revenueMetricsSummary, {
    name: "metrics-summary",
    description: "Current MRR / ARR / churn / ARPU for a project, in one currency",
    path: { project_id: 'project' },
  })

  endpoint(group, revenueGlobalEvents, {
    name: "global-events",
    description: "Org-wide revenue events across every project",
    query: {
      project_id: { kind: 'int', help: "Filter to a single project" },
      from: { kind: 'string', help: "Lower bound (inclusive), ISO-8601" },
      to: { kind: 'string', help: "Upper bound (inclusive), ISO-8601" },
      event_types: { kind: 'string', help: "Comma-separated event types (e.g. 'invoice.paid,charge.succeeded')" },
      limit: { kind: 'int', help: "Max rows, default 100, max 500" },
    },
  })

  endpoint(group, revenueMetricsGlobalMrr, {
    name: "metrics-global-mrr",
    description: "Org-wide MRR total, summed across every project in the install",
  })

  endpoint(group, revenueMetricsGlobalSummary, {
    name: "metrics-global-summary",
    description: "Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds, active subscriptions/customers, and..",
    query: {
      currency: { kind: 'string', help: "ISO-4217 currency code, default USD" },
    },
  })

  endpoint(group, revenueListProviders, {
    name: "list-providers",
    description: "List registered providers (what the UI needs to render the \"Connect\" dropdown + its wizard instructions)",
  })
}

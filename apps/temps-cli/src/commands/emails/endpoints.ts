// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps emails` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getEmailEvents,
  getEmailLinks,
  getEmailTracking,
  getGlobalEventStats,
  getGlobalEvents,
  resendEmail,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerEmailsEndpoints(program: Command): void {
  const group = topGroup(program, 'emails', "emails")

  endpoint(group, getGlobalEvents, {
    name: "tracking get-global-events",
    description: "GET /emails/events",
    query: {
      event_type: { kind: 'string', help: "Filter by event type (open, click)" },
      page: { kind: 'int', help: "Page number (default: 1)" },
      page_size: { kind: 'int', help: "Page size (default: 20, max: 100)" },
    },
  })

  endpoint(group, getGlobalEventStats, {
    name: "tracking get-global-event-stats",
    description: "GET /emails/events/stats",
  })

  endpoint(group, resendEmail, {
    name: "resend",
    description: "Resend a captured email",
    path: { id: 'string' },
  })

  endpoint(group, getEmailTracking, {
    name: "tracking get",
    description: "Get email tracking summary",
    path: { id: 'string' },
  })

  endpoint(group, getEmailEvents, {
    name: "tracking get-events",
    description: "Get email tracking events",
    path: { id: 'string' },
    query: {
      event_type: { kind: 'string', help: "Filter by event type (open, click)" },
    },
  })

  endpoint(group, getEmailLinks, {
    name: "tracking get-links",
    description: "Get tracked links for an email",
    path: { id: 'string' },
  })
}

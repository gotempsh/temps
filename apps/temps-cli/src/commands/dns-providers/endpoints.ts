// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps dns-provider` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  applyHostnameMode,
  previewHostnameMode,
  updateManagedDomain,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerDnsProvidersEndpoints(program: Command): void {
  const group = topGroup(program, 'dns-provider', "dns-provider")

  endpoint(group, updateManagedDomain, {
    name: "update-managed-domain",
    description: "Update a managed domain's settings (hostname mode, sync opt-in, auto-manage)",
    path: { provider_id: 'int', domain: 'string' },
    body: {
      auto_manage: { kind: 'bool', help: "Toggle automatic DNS management for this domain" },
      generated_hostname_mode: { kind: 'string', help: "'\"standard\"' or '\"flat\"'" },
      proxied_by_default: { kind: 'bool', help: "Default proxy mode for newly managed records; 'false' is an explicit override" },
      sync_generated_records: { kind: 'bool', help: "Toggle DNS record sync for this domain" },
    },
  })

  endpoint(group, applyHostnameMode, {
    name: "apply-hostname-mode",
    description: "Apply a hostname mode to a managed domain (persist + optional DNS sync + route reload)",
    path: { provider_id: 'int', domain: 'string' },
    body: {
      adopt_records: { kind: 'json', help: "Conflicting records, from the preview's 'conflicts', that the user confirmed the sync may adopt — one..." },
      mode: { kind: 'string!', help: "Target mode to apply: '\"standard\"' or '\"flat\"'" },
      skip_records: { kind: 'json', help: "Conflicting hostnames, from the preview's 'conflicts', that the user chose to leave untouched: the sync..." },
      sync_dns: { kind: 'bool', help: "Also reconcile the provider's DNS zone for the affected hostnames" },
    },
  })

  endpoint(group, previewHostnameMode, {
    name: "preview-hostname-mode",
    description: "Preview the impact of switching a managed domain's hostname mode",
    path: { provider_id: 'int', domain: 'string' },
    query: {
      mode: { kind: 'string!', help: "Target mode: standard|flat" },
      sync: { kind: 'bool', help: "Include DNS record changes" },
    },
  })
}

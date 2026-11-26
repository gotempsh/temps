// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps domains` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  getCertificateSettings,
  getChallengeToken,
  getDomainByHost,
  getDomainById,
  getOnDemandCertStatus,
  listOnDemandCerts,
  updateCertificateSettings,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerDomainsEndpoints(program: Command): void {
  const group = topGroup(program, 'domains', "domains")

  endpoint(group, getDomainByHost, {
    name: "get-by-host",
    description: "Get domain details by hostname",
    path: { hostname: 'string' },
  })

  endpoint(group, getOnDemandCertStatus, {
    name: "get-on-demand-cert-status",
    description: "Get on-demand TLS certificate status for a hostname",
    path: { hostname: 'string' },
  })

  endpoint(group, listOnDemandCerts, {
    name: "list-on-demand-certs",
    description: "List on-demand TLS certificate attempts",
    query: {
      page: { kind: 'int', help: "Page number (1-indexed)" },
      page_size: { kind: 'int', help: "Number of items per page (max 100)" },
    },
  })

  endpoint(group, getCertificateSettings, {
    name: "get-certificate-settings",
    description: "Get certificate settings",
    path: { domain_id: 'int' },
  })

  endpoint(group, updateCertificateSettings, {
    name: "update-certificate-settings",
    description: "Update certificate settings",
    path: { domain_id: 'int' },
    body: {
      alert_days: { kind: 'int', help: "Override the instance SSL expiry warning threshold; null inherits it" },
      alerts_enabled: { kind: 'bool', help: "Override instance SSL expiry alerts; null inherits it" },
      challenge_type: { kind: 'string!', help: "Validation method for future ACME orders" },
      renewal_days: { kind: 'int!', help: "Start ACME renewal this many days before expiry (1–90)" },
    },
  })

  endpoint(group, getDomainById, {
    name: "get-by-id",
    description: "Get domain by ID",
    path: { domain: 'int' },
  })

  endpoint(group, getChallengeToken, {
    name: "get-challenge-token",
    description: "Get challenge token for a domain (returns plain text token)",
    path: { domain: 'string' },
    output: 'bytes',
  })
}

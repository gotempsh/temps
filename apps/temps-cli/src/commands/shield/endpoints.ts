// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps shield` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  approveShieldProposal,
  createPlatformShieldEntry,
  createProjectShieldEntry,
  createShieldToken,
  deletePlatformShieldEntry,
  deleteProjectShieldEntry,
  getShieldSettings,
  getShieldSummary,
  listPlatformShieldEntries,
  listProjectShieldEntries,
  listShieldEvents,
  listShieldProposals,
  listShieldTokens,
  lookupShieldClient,
  rejectShieldProposal,
  revokeShieldToken,
  updatePlatformShieldEntry,
  updateProjectShieldEntry,
  updateShieldSettings,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerShieldEndpoints(program: Command): void {
  const group = topGroup(program, 'shield', "Shield: bot and abuse protection rules, entries and proposals")

  endpoint(group, listShieldEvents, {
    name: "list-events",
    description: "Recent Shield events for an environment",
    path: { project_id: 'project', environment_id: 'int' },
    query: {
      limit: { kind: 'int', help: "Events to return, 1 to 100; 20 by default" },
      kind: { kind: 'string', help: "Only this kind: 'verdict', 'challenge_issued', 'challenge_solved' or 'challenge_failed'" },
      verdict: { kind: 'string', help: "Only this verdict: 'block', 'challenge' or 'log'" },
      ip: { kind: 'string', help: "Only this client address, IPv4 or IPv6" },
      before: { kind: 'string', help: "Only events before this instant (the 'next_before' of the previous page)" },
    },
  })

  endpoint(group, lookupShieldClient, {
    name: "lookup-client",
    description: "Look up what Shield says about a client",
    path: { project_id: 'project', environment_id: 'int' },
    query: {
      ip: { kind: 'string', help: "A client address" },
      ja4: { kind: 'string', help: "A JA4 TLS fingerprint" },
    },
  })

  endpoint(group, getShieldSummary, {
    name: "get-summary",
    description: "Shield summary for an environment",
    path: { project_id: 'project', environment_id: 'int' },
    query: {
      window: { kind: 'string', help: "'1h', '24h' (the default) or '7d'", values: ["1h","24h","7d"] },
    },
  })

  endpoint(group, listShieldTokens, {
    name: "list-tokens",
    description: "List bypass tokens for an environment",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, createShieldToken, {
    name: "create-token",
    description: "Create a bypass token",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      name: { kind: 'string!', help: "What uses it: 'uptime monitor', 'CI'" },
      project_wide: { kind: 'bool', help: "Valid for every environment of the project, not only this one" },
    },
  })

  endpoint(group, revokeShieldToken, {
    name: "revoke-token",
    description: "Revoke a bypass token",
    path: { project_id: 'project', environment_id: 'int', token_id: 'int' },
    confirm: "This will revoke a bypass token.",
  })

  endpoint(group, listProjectShieldEntries, {
    name: "list-entries",
    description: "List a project's Shield entries",
    path: { project_id: 'project' },
  })

  endpoint(group, createProjectShieldEntry, {
    name: "create-entry",
    description: "Add a Shield entry to a project",
    path: { project_id: 'project' },
    body: {
      expires_at: { kind: 'string', help: "When the entry stops applying; omit for never" },
      ja4: { kind: 'string', help: "A JA4 fingerprint; exactly one of 'network' and 'ja4'" },
      network: { kind: 'string', help: "An address or CIDR range; exactly one of 'network' and 'ja4'" },
      note: 'string',
      verdict: { kind: 'string!', help: "The verdict an operator entry gives what it matches", values: ["allow","log","challenge","block"] },
    },
  })

  endpoint(group, deleteProjectShieldEntry, {
    name: "delete-entry",
    description: "Remove a project's Shield entry",
    path: { project_id: 'project', entry_id: 'int' },
    confirm: "This will remove a project's Shield entry.",
  })

  endpoint(group, updateProjectShieldEntry, {
    name: "update-entry",
    description: "Change a project's Shield entry",
    path: { project_id: 'project', entry_id: 'int' },
    body: {
      expires_at: 'string',
      note: 'string',
      verdict: { kind: 'string!', help: "The verdict an operator entry gives what it matches", values: ["allow","log","challenge","block"] },
    },
  })

  endpoint(group, getShieldSettings, {
    name: "get-settings",
    description: "Get the platform Shield settings",
  })

  endpoint(group, updateShieldSettings, {
    name: "update-settings",
    description: "Change the platform Shield settings",
    body: {
      share_observations: { kind: 'bool', help: "Whether what one environment's visitors did counts for every environment (D9): addresses and fingerprints..." },
    },
  })

  endpoint(group, listPlatformShieldEntries, {
    name: "list-platform-entries",
    description: "List platform Shield entries",
  })

  endpoint(group, createPlatformShieldEntry, {
    name: "create-platform-entry",
    description: "Add a platform Shield entry",
    body: {
      expires_at: { kind: 'string', help: "When the entry stops applying; omit for never" },
      ja4: { kind: 'string', help: "A JA4 fingerprint; exactly one of 'network' and 'ja4'" },
      network: { kind: 'string', help: "An address or CIDR range; exactly one of 'network' and 'ja4'" },
      note: 'string',
      verdict: { kind: 'string!', help: "The verdict an operator entry gives what it matches", values: ["allow","log","challenge","block"] },
    },
  })

  endpoint(group, deletePlatformShieldEntry, {
    name: "delete-platform-entry",
    description: "Remove a platform Shield entry",
    path: { entry_id: 'int' },
    confirm: "This will remove a platform Shield entry.",
  })

  endpoint(group, updatePlatformShieldEntry, {
    name: "update-platform-entry",
    description: "Change a platform Shield entry",
    path: { entry_id: 'int' },
    body: {
      expires_at: 'string',
      note: 'string',
      verdict: { kind: 'string!', help: "The verdict an operator entry gives what it matches", values: ["allow","log","challenge","block"] },
    },
  })

  endpoint(group, listShieldProposals, {
    name: "list-proposals",
    description: "List fleet learning's proposals",
    query: {
      status: { kind: 'string', help: "Only proposals in this state; all of them when omitted", values: ["pending","approved","rejected"] },
      q: { kind: 'string', help: "Only proposals whose address or fingerprint contains this text, ignoring case" },
      page: { kind: 'int', help: "Page number, from 1 (the default)" },
      page_size: { kind: 'int', help: "Proposals per page: 25 by default, at most 100" },
    },
  })

  endpoint(group, approveShieldProposal, {
    name: "approve-proposal",
    description: "Approve a proposal",
    path: { proposal_id: 'int' },
  })

  endpoint(group, rejectShieldProposal, {
    name: "reject-proposal",
    description: "Reject a proposal",
    path: { proposal_id: 'int' },
  })
}

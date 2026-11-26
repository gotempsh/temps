// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps settings` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createOidcProvider,
  createOidcRoleMapping,
  deleteOidcProvider,
  deleteOidcRoleMapping,
  getOidcProvider,
  getOnboarding,
  listOidcProviders,
  listOidcProviderUsers,
  listOidcRoleMappings,
  testOidcProvider,
  updateOidcProvider,
  updateOnboarding,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerSettingsEndpoints(program: Command): void {
  const group = topGroup(program, 'settings', "settings")

  endpoint(group, getOnboarding, {
    name: "onboarding get",
    description: "Show the install's first-run record: whether the welcome flow is finished, the steps left for later and why, and the default way in",
  })

  endpoint(group, updateOnboarding, {
    name: "onboarding update",
    description: "Change the install's first-run record",
    body: {
      completed: { kind: 'bool', help: "Mark the welcome flow finished (true) or reopen it (false)" },
      default_reach: { kind: 'json', help: 'How new apps are reached by default, e.g. {"audience":"public","way":"tunnel","naming":"wildcard","domain":"apps.example.com"}' },
      done: { kind: 'strings', help: "Mark these steps done", values: ["host","reach","agents","app"] },
      later: { kind: 'json', help: 'Leave steps for later with why, e.g. [{"step":"agents","reason":"no provider yet"}]' },
    },
  })

  endpoint(group, listOidcProviders, {
    name: "oidc list-providers",
    description: "List oidc providers",
  })

  endpoint(group, createOidcProvider, {
    name: "oidc create-provider",
    description: "Create oidc provider",
    body: {
      client_id: 'string!',
      client_secret: { kind: 'secret!', help: "A credential" },
      default_role: 'string',
      enabled: 'bool',
      group_claim: 'string',
      issuer_url: 'string!',
      jit_provisioning: 'bool',
      name: 'string!',
      role_claim: 'string',
      scopes: 'string',
      template: 'string',
      trust_idp_email: { kind: 'bool', help: "Defaults false" },
    },
  })

  endpoint(group, getOidcProvider, {
    name: "oidc get-provider",
    description: "Show one oidc provider",
    path: { provider_id: 'int' },
  })

  endpoint(group, deleteOidcProvider, {
    name: "oidc delete-provider",
    description: "Delete oidc provider",
    path: { provider_id: 'int' },
    confirm: "This will delete oidc provider.",
  })

  endpoint(group, updateOidcProvider, {
    name: "oidc update-provider",
    description: "Update oidc provider",
    path: { provider_id: 'int' },
    body: {
      client_id: 'string',
      client_secret: { kind: 'secret', help: "A credential" },
      default_role: 'string',
      enabled: 'bool',
      group_claim: 'string',
      issuer_url: 'string',
      jit_provisioning: 'bool',
      name: 'string',
      role_claim: 'string',
      scopes: 'string',
      template: 'string',
      trust_idp_email: 'bool',
    },
  })

  endpoint(group, listOidcRoleMappings, {
    name: "oidc list-role-mappings",
    description: "List oidc role mappings",
    path: { provider_id: 'int' },
  })

  endpoint(group, createOidcRoleMapping, {
    name: "oidc create-role-mapping",
    description: "Create oidc role mapping",
    path: { provider_id: 'int' },
    body: {
      idp_group: 'string!',
      priority: 'int!',
      role: 'string!',
    },
  })

  endpoint(group, testOidcProvider, {
    name: "oidc test-provider",
    description: "Test oidc provider",
    path: { provider_id: 'int' },
  })

  endpoint(group, listOidcProviderUsers, {
    name: "oidc list-provider-users",
    description: "List oidc provider users",
    path: { provider_id: 'int' },
    query: { page: 'int', page_size: 'int' },
  })

  endpoint(group, deleteOidcRoleMapping, {
    name: "oidc delete-role-mapping",
    description: "Delete oidc role mapping",
    path: { mapping_id: 'int' },
    confirm: "This will delete oidc role mapping.",
  })
}

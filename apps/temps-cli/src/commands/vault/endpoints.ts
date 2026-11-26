// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps vault` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  createAccessBundle,
  createAccessBundleGeneral,
  createAccessServiceGeneral,
  createAccessSession,
  createAccessSessionGeneral,
  createAccessVault,
  createAccessVaultGeneral,
  createVaultLink,
  deleteAccessBundleGeneral,
  deleteAccessServiceGeneral,
  deleteAccessVaultGeneral,
  deleteVaultLink,
  generalAccessCapability,
  getAccessServiceGeneral,
  getAccessSession,
  getAccessSessionGeneral,
  getAccessVault,
  getAccessVaultGeneral,
  getVaultEligibility,
  listAccessBundles,
  listAccessBundlesGeneral,
  listAccessServicesGeneral,
  listAccessSessions,
  listAccessSessionsGeneral,
  listAccessVaults,
  listAccessVaultsGeneral,
  listVaultLinks,
  projectAccessVaults,
  revokeAccessSession,
  revokeAccessSessionGeneral,
  sandboxAccessVaults,
  serviceCatalog,
  updateAccessBundle,
  updateAccessBundleGeneral,
  updateAccessServiceGeneral,
  updateAccessVault,
  updateAccessVaultGeneral,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerVaultEndpoints(program: Command): void {
  const group = topGroup(program, 'vault', "Access vaults, the services they hold and their sessions")

  endpoint(group, listAccessServicesGeneral, {
    name: "list-services",
    description: "List access services general",
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createAccessServiceGeneral, {
    name: "create-service",
    description: "Create access service general",
    query: {
      source_project_id: 'int',
    },
    body: {
      clear_credential: 'bool',
      credential: { kind: 'secret', help: "A credential" },
      enabled: { kind: 'bool!', help: "Allows requests to be saved; does not enable a network connection" },
      expected_version: { kind: 'int', help: "Required for updates; omit for creation" },
      max_ttl_seconds: { kind: 'int!', help: "Maximum lifetime of a request, between 60 and 86400 seconds" },
      name: 'string!',
      policy: { kind: 'json!', help: "Exact destination and request scope" },
      vault_id: { kind: 'string', help: "Add a newly created service to this vault in the same transaction" },
    },
  })

  endpoint(group, deleteAccessServiceGeneral, {
    name: "delete-service",
    description: "Delete access service general",
    path: { service_id: 'string' },
    confirm: "This will delete access service general.",
  })

  endpoint(group, getAccessServiceGeneral, {
    name: "get-service",
    description: "Get access service general",
    path: { service_id: 'string' },
  })

  endpoint(group, updateAccessServiceGeneral, {
    name: "update-service",
    description: "Update access service general",
    path: { service_id: 'string' },
    body: {
      clear_credential: 'bool',
      credential: { kind: 'secret', help: "A credential" },
      enabled: { kind: 'bool!', help: "Allows requests to be saved; does not enable a network connection" },
      expected_version: { kind: 'int', help: "Required for updates; omit for creation" },
      max_ttl_seconds: { kind: 'int!', help: "Maximum lifetime of a request, between 60 and 86400 seconds" },
      name: 'string!',
      policy: { kind: 'json!', help: "Exact destination and request scope" },
      vault_id: { kind: 'string', help: "Add a newly created service to this vault in the same transaction" },
    },
  })

  endpoint(group, listAccessSessionsGeneral, {
    name: "sessions list",
    description: "List access sessions general",
    query: {
      page: 'int',
      page_size: 'int',
      vault_id: 'string',
      sandbox_id: { kind: 'string', help: "Filter by the sandbox's public ID; ownership still applies" },
    },
  })

  endpoint(group, getAccessSessionGeneral, {
    name: "sessions get",
    description: "Get access session general",
    path: { session_id: 'string' },
  })

  endpoint(group, revokeAccessSessionGeneral, {
    name: "sessions revoke",
    description: "Revoke access session general",
    path: { session_id: 'string' },
    confirm: "This will revoke access session general.",
  })

  endpoint(group, listAccessVaultsGeneral, {
    name: "list",
    description: "List access vaults general",
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createAccessVaultGeneral, {
    name: "create",
    description: "Create access vault general",
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "Existing services in this project" },
    },
  })

  endpoint(group, deleteAccessVaultGeneral, {
    name: "delete",
    description: "Delete access vault general",
    path: { vault_id: 'string' },
    confirm: "This will delete access vault general.",
  })

  endpoint(group, getAccessVaultGeneral, {
    name: "get",
    description: "Get access vault general",
    path: { vault_id: 'string' },
    query: {
      sandbox_id: 'string',
    },
  })

  endpoint(group, updateAccessVaultGeneral, {
    name: "update",
    description: "Update access vault general",
    path: { vault_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "Existing services in this project" },
    },
  })

  endpoint(group, listAccessBundlesGeneral, {
    name: "list-bundles",
    description: "List access bundles general",
    path: { vault_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
      sandbox_id: 'string',
    },
  })

  endpoint(group, createAccessBundleGeneral, {
    name: "create-bundle",
    description: "Create access bundle general",
    path: { vault_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "A selection of this vault's services, not an authorization grant" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, deleteAccessBundleGeneral, {
    name: "delete-bundle",
    description: "Delete access bundle general",
    path: { vault_id: 'string', bundle_id: 'string' },
    confirm: "This will delete access bundle general.",
  })

  endpoint(group, updateAccessBundleGeneral, {
    name: "update-bundle",
    description: "Update access bundle general",
    path: { vault_id: 'string', bundle_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "A selection of this vault's services, not an authorization grant" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, getVaultEligibility, {
    name: "get-eligibility",
    description: "Get vault eligibility",
    path: { vault_id: 'string', sandbox_id: 'string' },
  })

  endpoint(group, listVaultLinks, {
    name: "list-links",
    description: "List vault links",
    path: { vault_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
      project_ids: 'string',
      sandbox_ids: 'string',
    },
  })

  endpoint(group, createVaultLink, {
    name: "create-link",
    description: "Create vault link",
    path: { vault_id: 'string' },
    body: {
      project_id: 'int',
      sandbox_id: 'string',
    },
  })

  endpoint(group, deleteVaultLink, {
    name: "delete-link",
    description: "Delete vault link",
    path: { vault_id: 'string', link_id: 'string' },
    confirm: "This will delete vault link.",
  })

  endpoint(group, createAccessSessionGeneral, {
    name: "sessions create",
    description: "Create access session general",
    path: { vault_id: 'string' },
    body: {
      bundle_id: 'string!',
      bundle_version: 'int!',
      consumer: 'json!',
      request_id: { kind: 'string!', help: "Stable for retries of this approval" },
      services: { kind: 'json!', help: "Exact policies the operator reviewed, one per bundle service (maximum 16)" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, listAccessSessions, {
    name: "project sessions list",
    description: "List access sessions",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
      vault_id: 'string',
      sandbox_id: { kind: 'string', help: "Filter by the sandbox's public ID; ownership still applies" },
    },
  })

  endpoint(group, getAccessSession, {
    name: "project sessions get",
    description: "Get access session",
    path: { project_id: 'project', session_id: 'string' },
  })

  endpoint(group, revokeAccessSession, {
    name: "project sessions revoke",
    description: "Revoke access session",
    path: { project_id: 'project', session_id: 'string' },
    confirm: "This will revoke access session.",
  })

  endpoint(group, listAccessVaults, {
    name: "project list",
    description: "List access vaults",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createAccessVault, {
    name: "project create",
    description: "Create access vault",
    path: { project_id: 'project' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "Existing services in this project" },
    },
  })

  endpoint(group, getAccessVault, {
    name: "project get",
    description: "Get access vault",
    path: { project_id: 'project', vault_id: 'string' },
  })

  endpoint(group, updateAccessVault, {
    name: "project update",
    description: "Update access vault",
    path: { project_id: 'project', vault_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "Existing services in this project" },
    },
  })

  endpoint(group, listAccessBundles, {
    name: "project list-bundles",
    description: "List access bundles",
    path: { project_id: 'project', vault_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, createAccessBundle, {
    name: "project create-bundle",
    description: "Create access bundle",
    path: { project_id: 'project', vault_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "A selection of this vault's services, not an authorization grant" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, updateAccessBundle, {
    name: "project update-bundle",
    description: "Update access bundle",
    path: { project_id: 'project', vault_id: 'string', bundle_id: 'string' },
    body: {
      expected_version: 'int',
      name: 'string!',
      service_ids: { kind: 'strings!', help: "A selection of this vault's services, not an authorization grant" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, createAccessSession, {
    name: "project create-session",
    description: "Create access session",
    path: { project_id: 'project', vault_id: 'string' },
    body: {
      bundle_id: 'string!',
      bundle_version: 'int!',
      consumer: 'json!',
      request_id: { kind: 'string!', help: "Stable for retries of this approval" },
      services: { kind: 'json!', help: "Exact policies the operator reviewed, one per bundle service (maximum 16)" },
      ttl_seconds: 'int!',
    },
  })

  endpoint(group, projectAccessVaults, {
    name: "project project",
    description: "Project access vaults",
    path: { project_id: 'project' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, sandboxAccessVaults, {
    name: "sandbox",
    description: "Sandbox access vaults",
    path: { sandbox_id: 'string' },
    query: {
      page: 'int',
      page_size: 'int',
    },
  })

  endpoint(group, generalAccessCapability, {
    name: "capability",
    description: "General access capability",
  })

  endpoint(group, serviceCatalog, {
    name: "service-catalog",
    description: "Service catalog",
    query: {
      page: 'int',
      page_size: 'int',
      search: 'string',
      id: 'string',
    },
  })
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps environments` commands that map one to one onto an API operation, declared
 * as typed tables (see `src/lib/endpoint.ts`).
 */

import type { Command } from 'commander'
import {
  applyEnvironmentReachability,
  checkCloudflareToken,
  createIngressLoginGrant,
  decideReachabilityConflict,
  deleteIngressConnector,
  getCertificateMethods,
  getConnectorEnvironmentHosts,
  getEnvironmentReachability,
  getIngressConnector,
  getPreviewDomainStatus,
  getProjectRuntimeHealth,
  getReachabilityChoices,
  getReachabilityRun,
  getRuntimeHealth,
  listIngressProviders,
  listReachabilityRuns,
  planEnvironmentReachability,
  resumeReachabilityRun,
  retryIngressHostnameDns,
  setConnectorEnvironmentHosts,
  sleepEnvironment,
  undoReachabilityRun,
  updateRuntimeHealth,
  wakeEnvironment,
} from '../../api/sdk.gen.js'
import { endpoint, topGroup } from '../../lib/endpoint.js'

export function registerEnvironmentsEndpoints(program: Command): void {
  const group = topGroup(program, 'environments', "environments")

  endpoint(group, getCertificateMethods, {
    name: "reachability get-certificate-methods",
    description: "Which ways to give a domain HTTPS work here",
    query: {
      domain: { kind: 'string', help: "The domain to judge, such as 'my-app.example.com' or '*.example.com'" },
    },
  })

  endpoint(group, checkCloudflareToken, {
    name: "reachability check-cloudflare-token",
    description: "Check which permissions a pasted Cloudflare API token has",
    body: {
      hostname: { kind: 'string', help: "A hostname the token is meant to serve, when the zone is not known: the longest zone the token reaches..." },
      token: { kind: 'secret!', help: "The token, as pasted" },
      zone: { kind: 'string', help: "The zone the token is meant for" },
    },
  })

  endpoint(group, deleteIngressConnector, {
    name: "ingress delete-connector",
    description: "Delete a tunnel at the provider and unpublish every hostname it carries",
    path: { id: 'int' },
    confirm: "This will delete a tunnel at the provider and unpublish every hostname it carries.",
  })

  endpoint(group, getIngressConnector, {
    name: "ingress get-connector",
    description: "Get one tunnel",
    path: { id: 'int' },
  })

  endpoint(group, getConnectorEnvironmentHosts, {
    name: "reachability get-connector-hosts",
    description: "What a tunnel publishes for environments, or would once turned on",
    path: { id: 'int' },
  })

  endpoint(group, setConnectorEnvironmentHosts, {
    name: "reachability set-connector-hosts",
    description: "Turn a tunnel's environment hosts on or off",
    path: { id: 'int' },
    body: {
      confirm: { kind: 'string', help: "The 'confirm_token' of the list that was shown and accepted: turning on publishes every host in it,..." },
      enabled: 'bool!',
    },
  })

  endpoint(group, retryIngressHostnameDns, {
    name: "ingress retry-hostname-dns",
    description: "Retry DNS setup for a failed hostname publication on its current tunnel",
    path: { id: 'int', hostname: 'string' },
  })

  endpoint(group, createIngressLoginGrant, {
    name: "create-ingress-login-grant",
    description: "Create ingress login grant",
    body: {
      host: { kind: 'string!', help: "The hostname, without a port" },
      next: { kind: 'string', help: "Path on the host to return to" },
      owner_id: 'int!',
      owner_kind: { kind: 'string!', help: "'environment' or 'sandbox' (a sandbox preview host)" },
      port: { kind: 'int', help: "The port the visitor reached the host on, when the proxy's request carries one (it leaves out the scheme's..." },
      scheme: { kind: 'string!', help: "'http' or 'https': how the visitor reached the host" },
      sig: 'string!',
      ts: { kind: 'int!', help: "Unix seconds the proxy signed the request at" },
    },
  })

  endpoint(group, listIngressProviders, {
    name: "ingress list-providers",
    description: "List the tunnel providers this control plane offers",
  })

  endpoint(group, sleepEnvironment, {
    name: "sleep",
    description: "Sleep an on-demand environment",
    path: { project_id: 'project', env_id: 'int' },
  })

  endpoint(group, wakeEnvironment, {
    name: "wake",
    description: "Wake a sleeping on-demand environment",
    path: { project_id: 'project', env_id: 'int' },
  })

  endpoint(group, getEnvironmentReachability, {
    name: "reachability get",
    description: "Whether visitors can reach each address of an environment",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, applyEnvironmentReachability, {
    name: "reachability apply",
    description: "Carry out one option of a stored plan",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      answers: { kind: 'json', help: "Answers to the option's fields by key. 'address', when given, must be the plan's; 'dns_provider_id' takes..." },
      confirmed: { kind: 'bool', help: "The user saw and accepted the option's 'confirmation'" },
      option: { kind: 'string!', help: "Which option of the plan to carry out", values: ["existing_tunnel","direct_managed_dns","new_tunnel","new_tunnel_with_token","direct_manual_dns","private_worker","tailscale_serve","tailscale_service","tailscale_service_with_credential","tunnel_environment_hosts"] },
      plan_id: { kind: 'string!', help: "The plan this acts on, from 'POST …/reachability/plan'" },
    },
  })

  endpoint(group, getReachabilityChoices, {
    name: "reachability get-choices",
    description: "Which audiences, and which ways in, can work for an environment",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, planEnvironmentReachability, {
    name: "reachability plan",
    description: "What *Make it reachable* would do for an address",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      address: { kind: 'string', help: "The hostname to make reachable; Temps suggests one without it" },
      audience: { kind: 'string', help: "Who an address is for", values: ["public","private","signed_in","tailnet_users","unsupported"] },
      gate: { kind: 'string', help: "Which gate an address has (D7)", values: ["temps","cloudflare_access","unsupported"] },
      principals: { kind: 'json', help: "For 'tailnet_users': the tailnet logins and tags it admits (ADR 065 D6)" },
    },
  })

  endpoint(group, listReachabilityRuns, {
    name: "reachability list-runs",
    description: "The newest runs of an environment",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, getReachabilityRun, {
    name: "reachability get-run",
    description: "One run, with the progress of each step and what it created",
    path: { project_id: 'project', environment_id: 'int', run_id: 'int' },
  })

  endpoint(group, decideReachabilityConflict, {
    name: "reachability decide-conflict",
    description: "Answer a conflict: a DNS record someone else changed while the run was setting it, or after it did (ADR..",
    path: { project_id: 'project', environment_id: 'int', run_id: 'int' },
    body: {
      decision: { kind: 'string!', help: "A user's answer to a conflict (ADR 064 D4), journaled with the entry", values: ["keep","overwrite"] },
      key: { kind: 'string!', help: "The journal entry in conflict" },
    },
  })

  endpoint(group, resumeReachabilityRun, {
    name: "reachability resume-run",
    description: "Carry on a waiting or interrupted run from its first step not done",
    path: { project_id: 'project', environment_id: 'int', run_id: 'int' },
    body: {
      answers: { kind: 'json', help: "'cloudflare_api_token', when the run had not saved the account yet" },
    },
  })

  endpoint(group, undoReachabilityRun, {
    name: "reachability undo-run",
    description: "Remove exactly what a run created, newest first",
    path: { project_id: 'project', environment_id: 'int', run_id: 'int' },
  })

  endpoint(group, getRuntimeHealth, {
    name: "health get-runtime",
    description: "Get runtime health",
    path: { project_id: 'project', environment_id: 'int' },
  })

  endpoint(group, updateRuntimeHealth, {
    name: "health update-runtime",
    description: "Update runtime health",
    path: { project_id: 'project', environment_id: 'int' },
    body: {
      enabled: 'bool!',
      paused: 'bool!',
      required_healthy: 'int',
    },
  })

  endpoint(group, getProjectRuntimeHealth, {
    name: "health get-project-runtime-health",
    description: "Get project runtime health",
    path: { project_id: 'project' },
  })

  endpoint(group, getPreviewDomainStatus, {
    name: "reachability get-preview-domain-status",
    description: "Whether the preview domain can be reached",
  })
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createRule,
  deleteRule,
  getEffectivePolicy as getEffectivePolicyOperation,
  getRule,
  listNodeModes,
  listRules,
  setNodeMode,
  setProfile,
} from '@/api/client'
import type {
  CreateNetworkPolicyRuleRequest as CreateNetworkPolicyRuleBody,
  EffectivePolicyResponse,
  ListRulesData,
  NetworkPolicyRuleListResponse,
  NetworkPolicyRuleResponse,
  NodeNetworkPolicyModeListResponse,
  NodeNetworkPolicyModeResponse,
  ProfileResponse,
  SetProfileRequest as SetProfileBody,
} from '@/api/client'

/** What a rule is attached to (ADR-052 §4 / ADR-055 §9). */
export type SubjectKind =
  'deployment' | 'sandbox' | 'service' | 'function' | 'job'
export const SUBJECT_KINDS: readonly SubjectKind[] = [
  'deployment',
  'sandbox',
  'service',
  'function',
  'job',
]
export type Direction = 'egress' | 'ingress'
export type PeerKind = 'service' | 'workload' | 'cidr' | 'host'
export const PEER_KINDS: readonly PeerKind[] = [
  'service',
  'workload',
  'cidr',
  'host',
]
export type Protocol = 'tcp' | 'udp' | 'any'
export type RuleAction = 'allow' | 'deny'
export type RuleOrigin = 'link' | 'operator' | 'system'
export type Profile = 'open' | 'restricted' | 'none'
export type NodeNetworkPolicyMode = 'legacy' | 'shadow' | 'enforce'

/**
 * A rule, as the server returns it (enums arrive as plain strings). `link`
 * rules are removed by unlinking, not deleted directly.
 */
export type NetworkPolicyRule = NetworkPolicyRuleResponse

/** A page of rules. */
export type NetworkPolicyRuleList = NetworkPolicyRuleListResponse

/** The list filters. `subject_kind` and `subject_id` go together. */
export interface ListRulesParams {
  subject_kind?: SubjectKind
  subject_id?: number
  origin?: RuleOrigin
  page?: number
  page_size?: number
}

/** The create request, with its enum fields narrowed to the known values. */
export type CreateNetworkPolicyRuleRequest = CreateNetworkPolicyRuleBody & {
  subject_kind: SubjectKind
  direction: Direction
  peer_kind: PeerKind
  protocol?: Protocol
  action: RuleAction
}

/** A subject's profile per direction, and whether it is the kind's default. */
export type ProfileSettings = ProfileResponse

/** Everything that applies to one subject. */
export type EffectivePolicy = EffectivePolicyResponse

/** Omit a direction for the kind's default. */
export type SetProfileRequest = SetProfileBody & {
  egress?: Profile
  ingress?: Profile
}

/** One node's enforcement mode. Node 0 is the control plane. */
export type NodeNetworkPolicyModeEntry = NodeNetworkPolicyModeResponse

/** Every node's enforcement mode. */
export type NodeNetworkPolicyModeList = NodeNetworkPolicyModeListResponse

// ── Query keys ─────────────────────────────────────────────────────────────

export const networkPolicyKeys = {
  all: ['network-policy'] as const,
  rules: (params: ListRulesParams) =>
    ['network-policy', 'rules', params] as const,
  rule: (ruleId: number) => ['network-policy', 'rule', ruleId] as const,
  subject: (kind: string, id: number) =>
    ['network-policy', 'subject', kind, id] as const,
  nodes: ['network-policy', 'nodes'] as const,
}

// ── Requests ───────────────────────────────────────────────────────────────

/** The query object sent for a list; empty filters are omitted. */
export function rulesQuery(
  params: ListRulesParams
): NonNullable<ListRulesData['query']> {
  const query: NonNullable<ListRulesData['query']> = {}
  if (params.subject_kind && params.subject_id !== undefined) {
    query.subject_kind = params.subject_kind
    query.subject_id = params.subject_id
  }
  if (params.origin) query.origin = params.origin
  if (params.page !== undefined) query.page = params.page
  if (params.page_size !== undefined) query.page_size = params.page_size
  return query
}

export async function listNetworkPolicyRules(
  params: ListRulesParams
): Promise<NetworkPolicyRuleList> {
  const { data } = await listRules({
    query: rulesQuery(params),
    throwOnError: true,
  })
  return data
}

export async function getNetworkPolicyRule(
  ruleId: number
): Promise<NetworkPolicyRule> {
  const { data } = await getRule({
    path: { rule_id: ruleId },
    throwOnError: true,
  })
  return data
}

export async function createNetworkPolicyRule(
  request: CreateNetworkPolicyRuleRequest
): Promise<NetworkPolicyRule> {
  const { data } = await createRule({ body: request, throwOnError: true })
  return data
}

export async function deleteNetworkPolicyRule(ruleId: number): Promise<void> {
  await deleteRule({ path: { rule_id: ruleId }, throwOnError: true })
}

export async function getEffectivePolicy(
  subjectKind: SubjectKind,
  subjectId: number
): Promise<EffectivePolicy> {
  const { data } = await getEffectivePolicyOperation({
    path: { subject_kind: subjectKind, subject_id: subjectId },
    throwOnError: true,
  })
  return data
}

export async function setSubjectProfile(
  subjectKind: SubjectKind,
  subjectId: number,
  request: SetProfileRequest
): Promise<ProfileSettings> {
  const { data } = await setProfile({
    path: { subject_kind: subjectKind, subject_id: subjectId },
    body: request,
    throwOnError: true,
  })
  return data
}

export async function listNodeNetworkPolicyModes(): Promise<NodeNetworkPolicyModeList> {
  const { data } = await listNodeModes({ throwOnError: true })
  return data
}

export async function setNodeNetworkPolicyMode(
  nodeId: number,
  mode: NodeNetworkPolicyMode
): Promise<NodeNetworkPolicyModeEntry> {
  const { data } = await setNodeMode({
    path: { node_id: nodeId },
    body: { mode },
    throwOnError: true,
  })
  return data
}

// ── Pure helpers ───────────────────────────────────────────────────────────

/** The raw values of the create form. */
export interface RuleFormValues {
  subject_kind: SubjectKind
  subject_id: string
  direction: Direction
  peer_kind: PeerKind
  peer_ref: string
  port_start: string
  port_end: string
  protocol: Protocol
  action: RuleAction
  description: string
}

/**
 * Why a hostname peer cannot be chosen. The server refuses such rules (409)
 * because nothing would enforce them; the form says so before sending.
 */
export const HOST_RULES_UNAVAILABLE =
  'Hostname rules are not available yet: no node runs the egress proxy that would enforce them. Use a CIDR block for the addresses instead.'

/**
 * Turn the create form into the request body, or the sentence explaining
 * what is wrong. The server validates again and its detail is shown verbatim.
 */
export function buildCreateRuleRequest(
  form: RuleFormValues
):
  | { ok: true; value: CreateNetworkPolicyRuleRequest }
  | { ok: false; error: string; field: keyof RuleFormValues } {
  const subjectId = Number(form.subject_id.trim())
  if (!Number.isInteger(subjectId) || subjectId <= 0) {
    return {
      ok: false,
      error:
        form.subject_kind === 'deployment'
          ? 'Subject ID must be the environment ID (a positive whole number).'
          : `Subject ID must be the ${form.subject_kind} ID (a positive whole number).`,
      field: 'subject_id',
    }
  }
  const peerRef = form.peer_ref.trim()
  if (peerRef === '') {
    return { ok: false, error: peerRefHint(form.peer_kind), field: 'peer_ref' }
  }
  if (form.peer_kind === 'host') {
    return { ok: false, error: HOST_RULES_UNAVAILABLE, field: 'peer_kind' }
  }
  const startText = form.port_start.trim()
  const endText = form.port_end.trim()
  let portStart: number | undefined
  let portEnd: number | undefined
  if (startText === '' && endText !== '') {
    return {
      ok: false,
      error: 'Set the first port when setting the last port.',
      field: 'port_start',
    }
  }
  if (startText !== '') {
    portStart = Number(startText)
    if (!isPort(portStart)) {
      return {
        ok: false,
        error: `Port ${startText} is not within 1-65535.`,
        field: 'port_start',
      }
    }
    if (endText !== '') {
      portEnd = Number(endText)
      if (!isPort(portEnd)) {
        return {
          ok: false,
          error: `Port ${endText} is not within 1-65535.`,
          field: 'port_end',
        }
      }
      if (portEnd < portStart) {
        return {
          ok: false,
          error: `Port range ${portStart}-${portEnd} ends before it starts.`,
          field: 'port_end',
        }
      }
    }
  }
  const description = form.description.trim()
  const value: CreateNetworkPolicyRuleRequest = {
    subject_kind: form.subject_kind,
    subject_id: subjectId,
    direction: form.direction,
    peer_kind: form.peer_kind,
    peer_ref: peerRef,
    protocol: form.protocol,
    action: form.action,
  }
  if (portStart !== undefined) value.port_start = portStart
  if (portEnd !== undefined) value.port_end = portEnd
  if (description !== '') value.description = description
  return { ok: true, value }
}

function isPort(value: number): boolean {
  return Number.isInteger(value) && value >= 1 && value <= 65535
}

/** What to type as the peer for each kind, with a concrete example. */
export function peerRefHint(kind: PeerKind): string {
  switch (kind) {
    case 'service':
      return 'Enter the service ID, e.g. 12.'
    case 'workload':
      return 'Enter the workload as <kind>/<id>, e.g. sandbox/42.'
    case 'cidr':
      return 'Enter a CIDR block, e.g. 10.0.0.0/8.'
    case 'host':
      return 'Enter a hostname, e.g. registry.example.test.'
  }
}

/** "every port", "443", "8000-8100", with the protocol. */
export function describePorts(rule: {
  port_start?: number | null
  port_end?: number | null
  protocol: string
  peer_kind?: string
}): string {
  const protocol = rule.protocol === 'any' ? '' : `/${rule.protocol}`
  if (rule.port_start == null) {
    return rule.peer_kind === 'host'
      ? `80, 443${protocol}`
      : `every port${protocol}`
  }
  if (rule.port_end == null || rule.port_end === rule.port_start) {
    return `${rule.port_start}${protocol}`
  }
  return `${rule.port_start}-${rule.port_end}${protocol}`
}

/** `sandbox 42`, `environment 7`: the subject the way an operator reads it. */
export function describeSubject(kind: string, id: number): string {
  return kind === 'deployment' ? `environment ${id}` : `${kind} ${id}`
}

/** Why a rule cannot be deleted from here, or `null` when it can. */
export function ruleDeleteBlockedReason(
  rule: NetworkPolicyRule
): string | null {
  if (rule.origin === 'link') {
    return 'This rule was created by a link; remove the link to remove it.'
  }
  return null
}

/** Why a stored rule is shown but not enforced, or `null` when it is. */
export function ruleNotEnforcedReason(rule: NetworkPolicyRule): string | null {
  return rule.enforceable
    ? null
    : 'Not enforced: hostname rules need the egress proxy, which no node runs yet.'
}

/**
 * Why `mode` cannot be selected on this node, or `null` when it can. Legacy
 * is always selectable, so a stale stored mode can be cleared.
 */
export function networkPolicyModeUnavailableReason(
  entry: NodeNetworkPolicyModeEntry,
  mode: NodeNetworkPolicyMode
): string | null {
  if (mode === 'legacy' || entry.supports_enforcement) return null
  return (
    entry.enforcement_unavailable_reason ??
    'This node cannot compute or apply network policies.'
  )
}

/** The node's network policy mode from the list, or `undefined`. */
export function nodeNetworkPolicyMode(
  list: NodeNetworkPolicyModeList | undefined,
  nodeId: number
): NodeNetworkPolicyModeEntry | undefined {
  return list?.nodes.find((node) => node.node_id === nodeId)
}

export function isNodeNetworkPolicyMode(
  value: string
): value is NodeNetworkPolicyMode {
  return value === 'legacy' || value === 'shadow' || value === 'enforce'
}

/** What enforcement status means for one subject's effective policy. */
export function effectivePolicyEnforcementText(
  policy: EffectivePolicy
): string {
  if (!policy.enforced) {
    return 'Shown but not enforced anywhere yet: every node is in Legacy or Shadow mode.'
  }
  const nodes = policy.enforcing_nodes
    .map((id) => (id === 0 ? 'the control plane' : `node ${id}`))
    .join(', ')
  return `Enforced on ${nodes}.`
}

/**
 * The profile request that changes one direction and keeps the other.
 * The server replaces both fields on every call (an omitted field means the
 * kind's default), so the other direction's override must be sent again.
 */
export function profileUpdate(
  current: ProfileSettings,
  direction: Direction,
  value: Profile | undefined
): SetProfileRequest {
  const kept = (profile: string, source: string): Profile | undefined =>
    source === 'override' && isProfile(profile) ? profile : undefined
  const request: SetProfileRequest = {}
  const egress =
    direction === 'egress' ? value : kept(current.egress, current.egress_source)
  const ingress =
    direction === 'ingress'
      ? value
      : kept(current.ingress, current.ingress_source)
  if (egress !== undefined) request.egress = egress
  if (ingress !== undefined) request.ingress = ingress
  return request
}

export function isProfile(value: string): value is Profile {
  return value === 'open' || value === 'restricted' || value === 'none'
}

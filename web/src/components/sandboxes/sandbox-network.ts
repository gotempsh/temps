// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EgressRule, SandboxEgressPolicy } from '@/api/client'

/** Server limits, mirrored so bad input never reaches the network. */
export const MAX_PREVIEW_CIDRS = 50
export const MAX_EGRESS_RULES = 100

/** Backends that can enforce a `restricted` egress policy. */
const RESTRICTED_EGRESS_BACKENDS = new Set(['docker'])

export function backendEnforcesRestrictedEgress(
  backend: string | null | undefined
): boolean {
  return RESTRICTED_EGRESS_BACKENDS.has(backend ?? 'docker')
}

const IPV4 =
  /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/

/**
 * Whether `value` looks like an IPv4/IPv6 address or CIDR. The server is the
 * judge (it also normalises); this only catches typos before submitting.
 */
export function looksLikeCidr(value: string): boolean {
  const slash = value.lastIndexOf('/')
  const address = slash === -1 ? value : value.slice(0, slash)
  const prefixText = slash === -1 ? null : value.slice(slash + 1)
  const isV4 = IPV4.test(address)
  const isV6 =
    !isV4 && address.includes(':') && /^[0-9a-fA-F:.]+$/.test(address)
  if (!isV4 && !isV6) return false
  if (prefixText === null) return true
  if (!/^\d{1,3}$/.test(prefixText)) return false
  return Number(prefixText) <= (isV4 ? 32 : 128)
}

/**
 * The allow-list typed one entry per line (commas also separate). Returns
 * the entries, or the first invalid one.
 */
export function parseCidrList(text: string): {
  entries: string[]
  invalid: string | null
} {
  const entries = text
    .split(/[\n,]/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0)
  const invalid = entries.find((entry) => !looksLikeCidr(entry)) ?? null
  return { entries, invalid }
}

/** One editable rule row. `ports` is the text the user typed. */
export interface EgressRuleDraft {
  kind: 'host' | 'cidr'
  value: string
  ports: string
}

/** `"443, 8443"` → `[443, 8443]`; empty → `[]` (80 and 443); bad → null. */
export function parsePorts(text: string): number[] | null {
  const parts = text
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
  const ports: number[] = []
  for (const part of parts) {
    if (!/^\d{1,5}$/.test(part)) return null
    const port = Number(part)
    if (port < 1 || port > 65535) return null
    if (!ports.includes(port)) ports.push(port)
  }
  return ports
}

const HOST =
  /^(\*\.)?([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)*[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/i

/** Why a rule row cannot be saved, or null when it can. */
export function ruleProblem(rule: EgressRuleDraft): string | null {
  const value = rule.value.trim()
  if (value.length === 0) {
    return rule.kind === 'host'
      ? 'Enter a hostname'
      : 'Enter an address or CIDR'
  }
  if (rule.kind === 'host' && (!HOST.test(value) || looksLikeCidr(value))) {
    return `"${value}" is not a hostname (use *.example.com for subdomains)`
  }
  if (rule.kind === 'cidr' && !looksLikeCidr(value)) {
    return `"${value}" is not an IP address or CIDR`
  }
  if (parsePorts(rule.ports) === null) {
    return 'Ports are numbers from 1 to 65535, separated by commas'
  }
  return null
}

export function draftsFromPolicy(
  policy: SandboxEgressPolicy
): EgressRuleDraft[] {
  return (policy.rules ?? []).map((rule) => ({
    kind: rule.host ? 'host' : 'cidr',
    value: rule.host ?? rule.cidr ?? '',
    ports: (rule.ports ?? []).join(', '),
  }))
}

/** The rules to submit; call only when no row has a `ruleProblem`. */
export function rulesFromDrafts(drafts: EgressRuleDraft[]): EgressRule[] {
  return drafts.map((draft) => {
    const value = draft.value.trim()
    const ports = parsePorts(draft.ports) ?? []
    return draft.kind === 'host'
      ? { host: value.toLowerCase(), ports }
      : { cidr: value, ports }
  })
}

/** A rule as a sentence: `api.example.com on 80, 443`. */
export function describeRule(rule: EgressRule): string {
  const ports = rule.ports && rule.ports.length > 0 ? rule.ports : [80, 443]
  return `${rule.host ?? rule.cidr ?? '?'} on ${ports.join(', ')}`
}

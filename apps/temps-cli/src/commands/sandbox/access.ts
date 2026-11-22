// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Preview access and egress policy for a sandbox (ADR-055 §11): the pure
// parsing and request building behind `sandbox preview-access`,
// `sandbox egress` and `sandbox create --egress-*`. The shapes come from the
// generated client so they cannot drift from the server's schema.
import type {
  EgressRule,
  SandboxEgressPolicy,
  SandboxPreviewAccess,
  UpdatePreviewAccessBody,
} from '../../api/types.gen.js'

/** Values `--access` accepts, as the API spells them. */
export const PREVIEW_ACCESS_VALUES: readonly SandboxPreviewAccess[] = [
  'temps_login',
  'password',
  'public',
]

/**
 * Validate `--access`. `login` is accepted as shorthand for `temps_login`,
 * and `-` as `_`, since both read naturally on a command line.
 */
export function parsePreviewAccess(value: string): SandboxPreviewAccess {
  const normalized = value.trim().toLowerCase().replace(/-/g, '_')
  const access = normalized === 'login' ? 'temps_login' : normalized
  const known = PREVIEW_ACCESS_VALUES.find((candidate) => candidate === access)
  if (!known) {
    throw new Error(
      `Unknown --access '${value}'. Expected one of: ${PREVIEW_ACCESS_VALUES.join(', ')} (or 'login')`,
    )
  }
  return known
}

/** Plain-language description of who can open the previews. */
export function describePreviewAccess(
  access: SandboxPreviewAccess | undefined,
  allowedCidrs: readonly string[] | undefined,
): string {
  const who =
    access === 'password'
      ? 'anyone with the preview password'
      : access === 'public'
        ? 'anyone with the URL'
        : // An absent value is a server older than ADR-055, where the
          // default was already Temps login.
          'signed-in Temps users with access to the sandbox'
  const cidrs = allowedCidrs ?? []
  return cidrs.length === 0 ? who : `${who}, from ${cidrs.join(', ')} only`
}

export interface PreviewAccessFlags {
  access?: string
  allowIp?: string[]
  anyIp?: boolean
}

/** Whether any flag that changes the setting was given. */
export function wantsPreviewAccessChange(flags: PreviewAccessFlags): boolean {
  return flags.access !== undefined || (flags.allowIp?.length ?? 0) > 0 || flags.anyIp === true
}

/**
 * Build the PUT body from the flags and the current setting. The endpoint
 * replaces both members, so whichever one the user did not pass keeps its
 * current value: changing the mode never clears an allow-list, and setting
 * an allow-list never changes the mode. `--allow-ip` replaces the list.
 */
export function buildPreviewAccessBody(
  flags: PreviewAccessFlags,
  current: { access: SandboxPreviewAccess; allowedCidrs: readonly string[] },
): UpdatePreviewAccessBody {
  const allowIp = flags.allowIp ?? []
  if (flags.anyIp && allowIp.length > 0) {
    throw new Error('--any-ip clears the allow-list; it cannot be combined with --allow-ip')
  }
  const access = flags.access !== undefined ? parsePreviewAccess(flags.access) : current.access
  let allowedCidrs: string[]
  if (flags.anyIp) {
    allowedCidrs = []
  } else if (allowIp.length > 0) {
    allowedCidrs = allowIp.map((value) => value.trim()).filter((value) => value.length > 0)
  } else {
    allowedCidrs = [...current.allowedCidrs]
  }
  return { access, allowed_cidrs: allowedCidrs }
}

/** True for an IPv4/IPv6 address or CIDR, false for a hostname. */
function isAddress(value: string): boolean {
  if (value.includes(':')) return true
  return /^\d{1,3}(\.\d{1,3}){3}(\/\d{1,2})?$/.test(value)
}

function parsePorts(raw: string, destination: string): number[] {
  return raw.split(',').map((part) => {
    const port = Number(part.trim())
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      throw new Error(`Invalid port '${part}' in '${destination}': expected 1-65535`)
    }
    return port
  })
}

/**
 * Parse one `--allow` destination into an egress rule.
 *
 * Forms: `api.example.com`, `*.example.com`, `api.example.com:443,8443`,
 * `203.0.113.0/24`, `203.0.113.0/24:5432`, `2606:4700::/32` and, with ports,
 * `[2606:4700::/32]:443`. No ports means 80 and 443, as on the server.
 * Hostname and CIDR validity is checked by the server, which owns the rules.
 */
export function parseEgressRule(value: string): EgressRule {
  const raw = value.trim()
  if (raw.length === 0) {
    throw new Error('Empty --allow destination')
  }

  let target: string
  let ports: number[] = []
  if (raw.startsWith('[')) {
    const close = raw.indexOf(']')
    if (close < 0) {
      throw new Error(`Unclosed '[' in '${value}': write IPv6 with ports as [addr/prefix]:port`)
    }
    target = raw.slice(1, close)
    const rest = raw.slice(close + 1)
    if (rest.length > 0) {
      if (!rest.startsWith(':')) {
        throw new Error(`Unexpected '${rest}' after ']' in '${value}'`)
      }
      ports = parsePorts(rest.slice(1), value)
    }
  } else if ((raw.match(/:/g) ?? []).length > 1) {
    // Bare IPv6: every colon belongs to the address.
    target = raw
  } else {
    const colon = raw.lastIndexOf(':')
    if (colon >= 0) {
      target = raw.slice(0, colon)
      ports = parsePorts(raw.slice(colon + 1), value)
    } else {
      target = raw
    }
  }

  if (target.length === 0) {
    throw new Error(`Missing host or CIDR in '${value}'`)
  }
  return isAddress(target) ? { cidr: target, ports } : { host: target.toLowerCase(), ports }
}

export interface EgressFlags {
  internet?: boolean
  denyAll?: boolean
  allow?: string[]
}

/** Whether any flag that changes the policy was given. */
export function wantsEgressChange(flags: EgressFlags): boolean {
  return flags.internet === true || flags.denyAll === true || (flags.allow?.length ?? 0) > 0
}

/**
 * Build the policy from the flags. Exactly one intent: `--internet`,
 * `--deny-all`, or one or more `--allow`. The policy replaces the current
 * one as a whole, so `--allow` lists every destination that stays reachable.
 */
export function buildEgressPolicy(flags: EgressFlags): SandboxEgressPolicy {
  const allow = flags.allow ?? []
  const intents = [flags.internet === true, flags.denyAll === true, allow.length > 0].filter(
    Boolean,
  ).length
  if (intents === 0) {
    throw new Error('Pass --internet, --deny-all, or one or more --allow <host|cidr>[:ports]')
  }
  if (intents > 1) {
    throw new Error('--internet, --deny-all and --allow are mutually exclusive')
  }
  if (flags.internet) return { mode: 'internet', rules: [] }
  return { mode: 'restricted', rules: allow.map(parseEgressRule) }
}

/** One line per rule, for display. */
export function describeEgressRule(rule: EgressRule): string {
  const target = rule.host ?? rule.cidr ?? '?'
  const ports = rule.ports && rule.ports.length > 0 ? rule.ports.join(', ') : '80, 443'
  return `${target}  (ports ${ports})`
}

/** Plain-language summary of a policy. */
export function describeEgressPolicy(policy: SandboxEgressPolicy | null | undefined): string {
  if (!policy || policy.mode === undefined || policy.mode === 'internet') {
    return 'internet (no restriction)'
  }
  const count = policy.rules?.length ?? 0
  return count === 0
    ? 'restricted: no outbound destinations allowed'
    : `restricted: ${count} allowed destination${count === 1 ? '' : 's'}`
}

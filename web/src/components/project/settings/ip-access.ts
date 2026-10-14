// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/** Mirrors the server limits in `ip_access::cidr_set`. */
export const MAX_CIDRS = 256
export const MAX_DESCRIPTION_LENGTH = 120

export type IpAccessMode = 'disabled' | 'allow' | 'deny'

/**
 * What the form edits. `inherit` is console-only: an environment with no list
 * of its own follows the project's, and choosing it removes the stored row.
 * It is distinct from `disabled`, which is a saved override that exempts the
 * environment from the project-wide list.
 */
export type IpAccessFormMode = IpAccessMode | 'inherit'

export interface CidrEntry {
  cidr: string
  description?: string | null
}

/** Ranges used by Tailscale for tailnet addresses (CGNAT block and ULA /48). */
export const TAILSCALE_PRESETS: CidrEntry[] = [
  { cidr: '100.64.0.0/10', description: 'Tailscale (IPv4)' },
  { cidr: 'fd7a:115c:a1e0::/48', description: 'Tailscale (IPv6)' },
]

const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/

function isIpv4(value: string): boolean {
  const match = IPV4.exec(value)
  if (!match) return false
  return match.slice(1).every((octet) => Number(octet) <= 255)
}

function isIpv6(value: string): boolean {
  if (!value.includes(':') || !/^[0-9a-fA-F:.]+$/.test(value)) return false
  if (value.split('::').length > 2) return false
  // A trailing dotted quad (::ffff:192.0.2.1) counts as two groups.
  const tail = value.split(':').pop() ?? ''
  const hasQuad = tail.includes('.')
  if (hasQuad && !isIpv4(tail)) return false
  const groups = value.split(':').filter((group) => group.length > 0)
  const count = groups.length + (hasQuad ? 1 : 0)
  const compressed = value.includes('::')
  if (compressed ? count > 7 : count !== 8) return false
  return groups.every(
    (group) => (hasQuad && group === tail) || /^[0-9a-fA-F]{1,4}$/.test(group)
  )
}

/**
 * Syntax-only check for an address or `address/prefix`. The server stays
 * authoritative (it also rejects host bits), this just catches typos early.
 */
export function cidrSyntaxError(raw: string): string | null {
  const value = raw.trim()
  if (value.length === 0) return 'Enter an address or CIDR range'
  const [address, prefix, ...rest] = value.split('/')
  if (rest.length > 0) return 'Use a single "/" between address and prefix'
  const v4 = isIpv4(address)
  const v6 = !v4 && isIpv6(address)
  if (!v4 && !v6) return `"${address}" is not a valid IPv4 or IPv6 address`
  if (prefix === undefined) return null
  if (!/^\d{1,3}$/.test(prefix)) return 'The prefix must be a number'
  const max = v4 ? 32 : 128
  if (Number(prefix) > max) {
    return `The prefix cannot exceed ${max} for ${v4 ? 'IPv4' : 'IPv6'}`
  }
  return null
}

const entrySchema = z.object({
  cidr: z.string(),
  description: z.string().optional(),
})

/**
 * Entries are validated only for modes that keep a list. `inherit` removes the
 * stored row, so rows the user left half-typed in the (hidden) editor must not
 * block that save.
 */
export const ipAccessFormSchema = z
  .object({
    mode: z.enum(['inherit', 'disabled', 'allow', 'deny']),
    cidrs: z.array(entrySchema),
  })
  .superRefine((value, ctx) => {
    if (value.mode === 'inherit') return

    if (value.cidrs.length > MAX_CIDRS) {
      ctx.addIssue({
        code: 'custom',
        path: ['cidrs'],
        message: `At most ${MAX_CIDRS} entries are allowed`,
      })
    }
    if (value.mode === 'allow' && value.cidrs.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['cidrs'],
        message:
          'An allow list needs at least one entry, otherwise nobody could reach the project',
      })
    }

    const seen = new Set<string>()
    value.cidrs.forEach((entry, index) => {
      const syntax = cidrSyntaxError(entry.cidr)
      if (syntax) {
        ctx.addIssue({
          code: 'custom',
          path: ['cidrs', index, 'cidr'],
          message: syntax,
        })
      }
      if ((entry.description ?? '').length > MAX_DESCRIPTION_LENGTH) {
        ctx.addIssue({
          code: 'custom',
          path: ['cidrs', index, 'description'],
          message: `At most ${MAX_DESCRIPTION_LENGTH} characters`,
        })
      }
      const key = entry.cidr.trim().toLowerCase()
      if (key && seen.has(key)) {
        ctx.addIssue({
          code: 'custom',
          path: ['cidrs', index, 'cidr'],
          message: `${entry.cidr.trim()} is listed twice`,
        })
      }
      seen.add(key)
    })
  })

export type IpAccessFormValues = z.infer<typeof ipAccessFormSchema>

/** Append presets that are not already listed, keeping what the user has. */
export function withPresets(
  current: CidrEntry[],
  presets: CidrEntry[]
): CidrEntry[] {
  const listed = new Set(
    current.map((entry) => entry.cidr.trim().toLowerCase())
  )
  const additions = presets.filter(
    (preset) => !listed.has(preset.cidr.toLowerCase())
  )
  return [...current, ...additions]
}

/** The presets that are not in the list yet. */
export function missingPresets(
  current: CidrEntry[],
  presets: CidrEntry[]
): CidrEntry[] {
  return withPresets(current, presets).slice(current.length)
}

/** True when every preset is already in the list (hides the "add" action). */
export function hasAllPresets(
  current: CidrEntry[],
  presets: CidrEntry[]
): boolean {
  return missingPresets(current, presets).length === 0
}

/**
 * Whether adding every missing preset keeps the list within `limit`. Adding
 * only some of them would leave the operator with a half-applied preset, so
 * the action is all-or-nothing.
 */
export function presetsFit(
  current: CidrEntry[],
  presets: CidrEntry[],
  limit: number = MAX_CIDRS
): boolean {
  return current.length + missingPresets(current, presets).length <= limit
}

/**
 * Which stored policy "Try an address" must evaluate when the draft is not
 * evaluated as given. An environment that is set to follow the project is
 * governed by the project's list, not by an override it is about to drop, so
 * the project endpoint answers; every other case asks the scope itself.
 */
export function probeScope(
  isEnvironment: boolean,
  mode: IpAccessFormMode
): 'project' | 'scope' {
  return isEnvironment && mode === 'inherit' ? 'project' : 'scope'
}

/** What "Try an address" evaluates for the form as it currently stands. */
export type ProbeDraft =
  | { kind: 'saved' }
  | {
      kind: 'draft'
      mode: IpAccessMode
      cidrs: { cidr: string; description: string | null }[]
    }
  | { kind: 'invalid'; message: string }

/**
 * Decide what a probe tests. A scope set to follow the project has no draft
 * of its own (the stored list answers); a valid draft is previewed as shown;
 * an invalid one is reported instead of silently testing the saved list,
 * which would answer a question nobody asked. An empty allow list is the one
 * invalid-to-save draft that is still previewed: the server answers it with
 * "everything blocked", which is exactly what the operator is about to cause.
 */
export function probeDraft(values: IpAccessFormValues): ProbeDraft {
  if (values.mode === 'inherit') return { kind: 'saved' }
  const toEntries = (cidrs: IpAccessFormValues['cidrs']) =>
    cidrs.map((entry) => ({
      cidr: entry.cidr.trim(),
      description: entry.description?.trim() || null,
    }))
  if (values.mode === 'allow' && values.cidrs.length === 0) {
    return { kind: 'draft', mode: 'allow', cidrs: [] }
  }
  const parsed = ipAccessFormSchema.safeParse(values)
  if (!parsed.success) {
    return {
      kind: 'invalid',
      message:
        parsed.error.issues[0]?.message ??
        'The list has entries that are not valid',
    }
  }
  return {
    kind: 'draft',
    mode: values.mode as IpAccessMode,
    cidrs: toEntries(values.cidrs),
  }
}

/** Single host form of an address, for the "add my IP" button. */
export function hostCidr(ip: string): string {
  return ip.includes(':') ? `${ip}/128` : `${ip}/32`
}

export function modeLabel(mode: IpAccessFormMode): string {
  switch (mode) {
    case 'inherit':
      return 'Follow the project list'
    case 'allow':
      return 'Only allow listed addresses'
    case 'deny':
      return 'Block listed addresses'
    case 'disabled':
      return 'Off'
  }
}

/** The mode a scope's form starts in for the policy the server returned. */
export function initialFormMode(
  policy: { configured: boolean; mode: IpAccessMode },
  isEnvironment: boolean
): IpAccessFormMode {
  return isEnvironment && !policy.configured ? 'inherit' : policy.mode
}

/** Entries are only meaningful for a list that is kept (anything but inherit). */
export function showsEntries(mode: IpAccessFormMode): boolean {
  return mode !== 'inherit'
}

/** One-line description of what the saved policy does, for status badges. */
export function policySummary(mode: IpAccessMode, count: number): string {
  const noun = count === 1 ? 'range' : 'ranges'
  switch (mode) {
    case 'allow':
      return `Only ${count} ${noun} can reach this`
    case 'deny':
      return `${count} ${noun} blocked`
    case 'disabled':
      return 'Open to everyone'
  }
}

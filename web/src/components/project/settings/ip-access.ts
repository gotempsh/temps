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
  cidr: z.string().superRefine((value, ctx) => {
    const message = cidrSyntaxError(value)
    if (message) ctx.addIssue({ code: 'custom', message })
  }),
  description: z
    .string()
    .max(MAX_DESCRIPTION_LENGTH, `At most ${MAX_DESCRIPTION_LENGTH} characters`)
    .optional(),
})

export const ipAccessFormSchema = z
  .object({
    mode: z.enum(['inherit', 'disabled', 'allow', 'deny']),
    cidrs: z
      .array(entrySchema)
      .max(MAX_CIDRS, `At most ${MAX_CIDRS} entries are allowed`),
  })
  .superRefine((value, ctx) => {
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

/** True when every preset is already in the list (hides the "add" action). */
export function hasAllPresets(
  current: CidrEntry[],
  presets: CidrEntry[]
): boolean {
  return withPresets(current, presets).length === current.length
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

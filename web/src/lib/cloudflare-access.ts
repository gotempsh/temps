// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The separate Cloudflare token Cloudflare Access needs (ADR 065 D9): what
// it must have, how a pasted one is validated before it is checked, and
// how a saved one is described.

import { z } from 'zod'

export const CLOUDFLARE_API_TOKENS_URL =
  'https://dash.cloudflare.com/profile/api-tokens'

/** Where the console sends people to add an Access token. */
export const ACCESS_TOKENS_SETUP_PATH = '/dns-providers#cloudflare-access'

/** The token's permissions, in Cloudflare's own words, and what each is for. */
export const ACCESS_TOKEN_PERMISSIONS = [
  {
    permission: 'Account · Access: Apps and Policies · Edit',
    purpose: 'find the Access app in front of an address, or create one',
  },
  {
    permission:
      'Account · Access: Organizations, Identity Providers, and Groups · Read',
    purpose: "read your Access team's domain, which signs its sign-ins",
  },
] as const

/** Shown before the token is saved: Cloudflare has no narrower form. */
export const ACCESS_SCOPE_NOTE =
  'Both permissions are account-wide: this token can change or remove the protection of every Access app in the account, not only the ones Temps creates. That is why it is a token of its own, separate from the one that runs your tunnels.'

/** A Cloudflare account id: 32 hexadecimal characters. */
export function isAccountId(value: string): boolean {
  return /^[0-9a-f]{32}$/i.test(value.trim())
}

export const accessTokenFormSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Give the token a name')
    .max(255, 'At most 255 characters'),
  account_id: z
    .string()
    .trim()
    .refine(
      isAccountId,
      'The account id is 32 characters, from the Cloudflare dashboard URL or the account overview'
    ),
  token: z.string().trim().min(20, 'Paste the whole token'),
})

export type AccessTokenFormValues = z.infer<typeof accessTokenFormSchema>

/** How many addresses a saved token protects, in words. */
export function protectsLabel(addresses: number): string {
  if (addresses === 0) return 'Protects no address yet'
  return addresses === 1
    ? 'Protects 1 address'
    : `Protects ${addresses} addresses`
}

/** Why a saved token cannot be deleted now, or `null` when it can. */
export function deleteBlockedReason(addresses: number): string | null {
  if (addresses === 0) return null
  return `${protectsLabel(addresses)}. Switch ${addresses === 1 ? 'it' : 'them'} to the Temps sign-in, or remove ${addresses === 1 ? 'it' : 'them'}, before deleting this token.`
}

/** What a listener has of the team's signing keys. */
export function keysLabel(keyCount: number, fetchedAt?: string | null): string {
  if (!fetchedAt)
    return 'Signing keys not fetched yet: Temps retries every hour.'
  return keyCount === 1 ? '1 signing key' : `${keyCount} signing keys`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/** Lifetimes offered for a share link; the API clamps to 24 hours. */
export const PREVIEW_LINK_TTLS = [
  { seconds: 900, label: '15 minutes' },
  { seconds: 3600, label: '1 hour' },
  { seconds: 14_400, label: '4 hours' },
  { seconds: 86_400, label: '24 hours' },
] as const

export const DEFAULT_PREVIEW_LINK_TTL_SECONDS = 3600

export const previewLinkFormSchema = z.object({
  port: z
    .string()
    .trim()
    .refine(
      (value) =>
        /^\d+$/.test(value) && Number(value) >= 1 && Number(value) <= 65_535,
      'Port must be a number from 1 to 65535'
    ),
  path: z
    .string()
    .trim()
    .refine(
      (value) =>
        value === '' || (value.startsWith('/') && !value.startsWith('//')),
      'Path must start with a single /'
    ),
  ttlSeconds: z.string(),
})

export type PreviewLinkFormValues = z.infer<typeof previewLinkFormSchema>

export function toPreviewLinkBody(values: PreviewLinkFormValues): {
  port: number
  path?: string
  ttl_seconds: number
} {
  const path = values.path.trim()
  return {
    port: Number(values.port),
    ...(path ? { path } : {}),
    ttl_seconds: Number(values.ttlSeconds),
  }
}

/**
 * Why a link cannot be created right now, or `null`. Minting needs the
 * sandbox's preview password: without one the API answers 409, and a link
 * would have nothing to gate.
 */
export function previewLinkBlockedReason(options: {
  /** Preview URLs are configured on this install. */
  configured: boolean
  /** The sandbox runs on a worker node, which has no preview URLs yet. */
  onWorkerNode: boolean
  hasPassword: boolean
  destroyed: boolean
  running: boolean
}): string | null {
  if (options.destroyed) return 'This sandbox has been destroyed.'
  if (options.onWorkerNode) {
    return 'Preview URLs are not available yet for sandboxes on worker nodes. Create the sandbox on the control plane to share a preview.'
  }
  if (!options.configured) {
    return 'Preview URLs are not configured on this install, so there is nothing to link to. Configure a preview domain in the platform settings first.'
  }
  if (!options.hasPassword) {
    return 'Set a preview password first. Share links are minted against it, and without one the preview has nothing to gate.'
  }
  if (!options.running) {
    return 'The sandbox is not running, so a link would have nothing to open. Start it first.'
  }
  return null
}

/** `expires_at` from the API is Unix seconds. */
export function previewLinkExpiry(expiresAt: number, now: number): string {
  const remaining = expiresAt * 1000 - now
  if (remaining <= 0) return 'expired'
  const minutes = Math.round(remaining / 60_000)
  if (minutes < 1) return 'expires in under a minute'
  if (minutes < 60) return `expires in ${minutes} min`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest === 0
    ? `expires in ${hours} h`
    : `expires in ${hours} h ${rest} min`
}

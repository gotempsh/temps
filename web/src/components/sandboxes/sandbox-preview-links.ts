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

/**
 * When a link stops working, as a wall-clock time. `expires_at` from the API
 * is Unix seconds. An absolute time never goes stale on screen the way a
 * "in 1 h" label computed once would.
 */
export function previewLinkExpiry(
  expiresAt: number,
  options: { locale?: string; timeZone?: string } = {}
): string {
  // Date and time are formatted separately and joined here: a combined
  // `toLocaleString` uses a locale-data-version-dependent connector
  // ("Jan 1, 12:30 PM" in one ICU release, "Jan 1 at 12:30 PM" in another).
  const at = new Date(expiresAt * 1000)
  const day = at.toLocaleDateString(options.locale, {
    month: 'short',
    day: 'numeric',
    timeZone: options.timeZone,
  })
  const time = at.toLocaleTimeString(options.locale, {
    hour: 'numeric',
    minute: '2-digit',
    timeZone: options.timeZone,
  })
  const formatted = `${day}, ${time}`
  return `expires ${formatted}`
}

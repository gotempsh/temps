// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Stacking tiers for every portalled overlay in the console.
 *
 * Radix portals each overlay to `document.body`, so stacking is decided only
 * by z-index and DOM order. Overlays that are opened *from* another overlay
 * must sit in a higher tier than the overlay they were opened from, otherwise
 * they render underneath it and cannot be clicked (e.g. an "Archive flag?"
 * confirmation opened from an edit Sheet).
 *
 * Opening order, lowest to highest:
 *
 *   sheet -> dialog -> alert dialog -> popover / select / dropdown -> tooltip
 *
 * All tiers stay above in-page chrome (sticky headers, the sidebar, the
 * product tour at z-100) and below the toaster, which sonner pins at a
 * z-index of 999999999.
 *
 * Class names are written out in full so Tailwind's source scanner emits
 * them. Do not build them from template strings.
 */
export const OVERLAY_LAYERS = {
  sheetOverlay: 'z-[9000]',
  sheetContent: 'z-[9010]',
  dialogOverlay: 'z-[9100]',
  dialogContent: 'z-[9110]',
  alertDialogOverlay: 'z-[9200]',
  alertDialogContent: 'z-[9210]',
  popover: 'z-[9300]',
  tooltip: 'z-[9400]',
} as const

export type OverlayLayer = keyof typeof OVERLAY_LAYERS

/** Numeric z-index for a tier, used to assert ordering in tests. */
export function overlayLayerIndex(layer: OverlayLayer): number {
  const match = /^z-\[(\d+)\]$/.exec(OVERLAY_LAYERS[layer])
  return match ? Number(match[1]) : Number.NaN
}

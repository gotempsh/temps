// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type React from 'react'

// Shared Recharts <Tooltip> styles.
//
// The theme tokens in globals.css are full oklch() colors (Tailwind v4), so
// they must be consumed as `var(--token)` — wrapping them in hsl() produces
// an invalid color and the tooltip renders with a transparent background.
// Import these instead of hand-rolling contentStyle per chart.
export const TOOLTIP_CONTENT_STYLE: React.CSSProperties = {
  fontSize: 12,
  backgroundColor: 'var(--popover)',
  border: '1px solid var(--border)',
  borderRadius: 8,
  color: 'var(--popover-foreground)',
  boxShadow: '0 4px 12px -2px rgb(0 0 0 / 0.4)',
  padding: '6px 10px',
}

export const TOOLTIP_LABEL_STYLE: React.CSSProperties = {
  color: 'var(--muted-foreground)',
  fontSize: 11,
  marginBottom: 2,
}

// Multi-day range charts key their X axis on the raw epoch-ms timestamp, not
// an "HH:mm" string — recharts' category axis picks the tooltip's data point
// by matching the dataKey value, so points from different days that format
// to the same "HH:mm" (e.g. 08:00 today and 08:00 a week ago) would collide
// and the tooltip could snap to the wrong day's value. Ticks stay time-only;
// the tooltip label adds the date so same-time points stay distinguishable.

/** Format an epoch-ms timestamp for chart axis ticks. */
export function formatChartTick(ts: number): string {
  return new Date(ts).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
  })
}

/** Format an epoch-ms timestamp for a chart tooltip's label row. */
export function formatChartTooltipLabel(ts: number): string {
  return new Date(ts).toLocaleString([], {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

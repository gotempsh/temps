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

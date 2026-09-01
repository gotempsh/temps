// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  siCaprover,
  siCoolify,
  siPortainer,
  siDocker,
  siFlydotio,
  siKubernetes,
  siNetlify,
  siRailway,
  siRender,
  siVercel,
} from 'simple-icons'
import { Server } from 'lucide-react'

/** Brand marks for import sources, keyed by the backend source id. */
const BRAND_ICONS: Record<string, { path: string; hex: string; title: string }> =
  {
    docker: siDocker,
    kubernetes: siKubernetes,
    coolify: siCoolify,
    caprover: siCaprover,
    portainer: siPortainer,
    vercel: siVercel,
    railway: siRailway,
    netlify: siNetlify,
    render: siRender,
    fly: siFlydotio,
  }

/** Near-black brand colors are invisible in dark mode — use the theme
 *  foreground for those instead of the fixed brand hex. */
function isNearBlack(hex: string): boolean {
  const value = parseInt(hex, 16)
  const r = (value >> 16) & 0xff
  const g = (value >> 8) & 0xff
  const b = value & 0xff
  return 0.299 * r + 0.587 * g + 0.114 * b < 60
}

interface SourceLogoProps {
  source: string
  className?: string
}

/** Logo for an import source; falls back to a generic server icon. */
export function SourceLogo({ source, className }: SourceLogoProps) {
  const icon = BRAND_ICONS[source]
  if (icon) {
    const nearBlack = isNearBlack(icon.hex)
    return (
      <svg
        viewBox="0 0 24 24"
        role="img"
        aria-label={icon.title}
        className={nearBlack ? `${className ?? ''} fill-foreground` : className}
        style={nearBlack ? undefined : { fill: `#${icon.hex}` }}
      >
        <path d={icon.path} />
      </svg>
    )
  }
  if (source === 'kamal') {
    // No simple-icons entry for Kamal — recognizable lettermark badge
    return (
      <svg viewBox="0 0 24 24" role="img" aria-label="Kamal" className={className}>
        <rect x="1" y="1" width="22" height="22" rx="5" className="fill-foreground" />
        <text
          x="12"
          y="16.5"
          textAnchor="middle"
          fontSize="12"
          fontWeight="700"
          fontFamily="ui-sans-serif, system-ui"
          className="fill-background"
        >
          K
        </text>
      </svg>
    )
  }
  if (source === 'dokploy') {
    // Dokploy has no simple-icons entry yet — recognizable lettermark badge
    return (
      <svg
        viewBox="0 0 24 24"
        role="img"
        aria-label="Dokploy"
        className={className}
      >
        <rect x="1" y="1" width="22" height="22" rx="5" className="fill-foreground" />
        <text
          x="12"
          y="16.5"
          textAnchor="middle"
          fontSize="13"
          fontWeight="700"
          fontFamily="ui-sans-serif, system-ui"
          className="fill-background"
        >
          D
        </text>
      </svg>
    )
  }
  return <Server className={className} />
}

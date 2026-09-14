// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Monitor, Smartphone, Tablet, CircleHelp } from 'lucide-react'
import { BrowserLogo } from '@/components/ui/browser-logo'

export function OsIcon({ os, size = 20 }: { os: string; size?: number }) {
  // Use emoji flags for well-known OSes, with lucide fallback
  const osLower = os.toLowerCase()

  if (osLower.includes('windows')) {
    return (
      <span
        style={{ fontSize: size - 4, lineHeight: `${size}px` }}
        role="img"
        aria-label="Windows"
      >
        🪟
      </span>
    )
  }
  if (
    osLower.includes('mac') ||
    osLower === 'ios' ||
    osLower.includes('iphone') ||
    osLower.includes('ipad')
  ) {
    return (
      <span
        style={{ fontSize: size - 4, lineHeight: `${size}px` }}
        role="img"
        aria-label="Apple"
      >
        🍎
      </span>
    )
  }
  if (
    osLower.includes('linux') ||
    osLower.includes('ubuntu') ||
    osLower.includes('debian') ||
    osLower.includes('fedora')
  ) {
    return (
      <span
        style={{ fontSize: size - 4, lineHeight: `${size}px` }}
        role="img"
        aria-label="Linux"
      >
        🐧
      </span>
    )
  }
  if (osLower.includes('android')) {
    return (
      <Smartphone
        className="text-muted-foreground"
        style={{ width: size, height: size }}
      />
    )
  }
  if (osLower.includes('chrome')) {
    return (
      <Tablet
        className="text-muted-foreground"
        style={{ width: size, height: size }}
      />
    )
  }

  return (
    <Monitor
      className="text-muted-foreground"
      style={{ width: size, height: size }}
    />
  )
}

export function DeviceIcon({ device }: { device: string }) {
  const Icon =
    ({ desktop: Monitor, mobile: Smartphone, tablet: Tablet } as const)[
      device.toLowerCase() as 'desktop' | 'mobile' | 'tablet'
    ] || CircleHelp
  return (
    <Icon
      aria-hidden="true"
      className="h-5 w-5 shrink-0 text-muted-foreground"
    />
  )
}

export function TechnologyIcon({
  dimension,
  value,
}: {
  dimension?: string
  value: string
}) {
  if (dimension === 'browser')
    return <BrowserLogo browser={value} size={20} className="shrink-0" />
  if (dimension === 'operating_system') return <OsIcon os={value} />
  if (dimension === 'device_type') return <DeviceIcon device={value} />
  return null
}

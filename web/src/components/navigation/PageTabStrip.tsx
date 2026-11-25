// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, type ComponentType, type ReactNode } from 'react'
import { Link } from 'react-router'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import { cn } from '@/lib/utils'

export interface PageTab {
  key: string
  title: string
  icon: ComponentType<{ className?: string }>
  to: string
  active: boolean
  featureKey?: string
  /** The quick tour's anchor for this tab. */
  tour?: string
}

export type PageTabLevel = 'primary' | 'secondary' | 'tertiary'

const STRIP: Record<PageTabLevel, string> = {
  primary: 'min-h-10 gap-5 border-b',
  secondary: 'min-h-8 gap-1',
  tertiary: 'min-h-7 gap-4',
}

const TAB: Record<PageTabLevel, string> = {
  primary: 'min-h-10 border-b-2 border-transparent py-3 text-sm',
  secondary: 'min-h-8 rounded-md px-2.5 py-1 text-sm',
  tertiary: 'min-h-7 text-xs',
}

const TAB_ACTIVE: Record<PageTabLevel, string> = {
  primary: 'border-foreground text-foreground',
  secondary: 'bg-muted text-foreground',
  tertiary: 'text-foreground underline decoration-1 underline-offset-[6px]',
}

/**
 * An area's pages as underline tabs above the content. They navigate between
 * URLs, so they are links in a `nav` (new tab, copy link and history work),
 * styled like the shared underline `Tabs`. One page renders no strip.
 *
 * On a narrow screen the strip scrolls sideways; the current tab is scrolled
 * into view so a page reached by link does not open with its own tab hidden.
 */
export function PageTabStrip({
  label,
  tabs,
  level = 'primary',
  icons = level !== 'tertiary',
  trailing,
  className,
}: {
  /** Names the strip for assistive technology, such as "Errors pages". */
  label: string
  tabs: PageTab[]
  /**
   * `secondary` for the pages behind one tab of the strip above, and
   * `tertiary` for the pages behind one of those: each lighter than the
   * last and with no rule of its own, so the rows read as one piece of
   * chrome.
   */
  level?: PageTabLevel
  /** Off where the strip holds many tabs and their names say enough. */
  icons?: boolean
  /** A control after the last tab, such as what else can be added. */
  trailing?: ReactNode
  className?: string
}) {
  const navRef = useRef<HTMLElement>(null)
  const current = tabs.find((tab) => tab.active)?.key
  useEffect(() => {
    const nav = navRef.current
    const active = nav?.querySelector<HTMLElement>('[aria-current="page"]')
    if (!nav || !active) return
    const strip = nav.getBoundingClientRect()
    const box = active.getBoundingClientRect()
    // Horizontal only: scrollIntoView would also scroll the page.
    if (box.left < strip.left || box.right > strip.right)
      nav.scrollLeft += box.left - strip.left - (strip.width - box.width) / 2
  }, [current])
  if (tabs.length < 2 && !trailing) return null
  return (
    <nav
      ref={navRef}
      aria-label={label}
      className={cn(
        'mb-6 flex w-full items-center overflow-x-auto text-muted-foreground',
        STRIP[level],
        className
      )}
    >
      {tabs.map((tab) => (
        <Link
          key={tab.key}
          to={tab.to}
          aria-current={tab.active ? 'page' : undefined}
          data-tour={tab.tour}
          className={cn(
            'inline-flex shrink-0 items-center gap-2 whitespace-nowrap font-medium hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
            TAB[level],
            tab.active && TAB_ACTIVE[level]
          )}
        >
          {/* Labels only on a phone, like `TabsTrigger`. */}
          {icons && (
            <tab.icon
              className="size-4 shrink-0 max-sm:hidden"
              aria-hidden="true"
            />
          )}
          {tab.title}
          {tab.featureKey && (
            <FeatureMaturityBadge featureKey={tab.featureKey} compact />
          )}
        </Link>
      ))}
      {trailing}
    </nav>
  )
}

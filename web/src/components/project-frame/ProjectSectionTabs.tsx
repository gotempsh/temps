// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import { Link, useSearchParams } from 'react-router'
import { cn } from '@/lib/utils'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import { projectRoot, type ProjectNavLocation } from '@/lib/project-navigation'

/**
 * A section's pages as underline tabs above the content. These navigate
 * between URLs, so they are links in a `nav` (new tab, copy link and history
 * work), styled like the shared underline `Tabs`. Each keeps the selected
 * environment. Sections with a single page render no strip.
 *
 * On a narrow screen the strip scrolls sideways; the current tab is scrolled
 * into view so a page reached by link (Check-ins, the last Monitoring tab)
 * does not open with its own tab hidden.
 */
export function ProjectSectionTabs({
  projectSlug,
  location,
}: {
  projectSlug: string
  location: ProjectNavLocation
}) {
  const [searchParams] = useSearchParams()
  const { section, tab } = location
  const navRef = useRef<HTMLElement>(null)
  useEffect(() => {
    const nav = navRef.current
    const current = nav?.querySelector<HTMLElement>('[aria-current="page"]')
    if (!nav || !current) return
    const strip = nav.getBoundingClientRect()
    const box = current.getBoundingClientRect()
    // Horizontal only: scrollIntoView would also scroll the page.
    if (box.left < strip.left || box.right > strip.right)
      nav.scrollLeft += box.left - strip.left - (strip.width - box.width) / 2
  }, [tab.url])
  if (section.tabs.length < 2) return null
  const env = searchParams.get(ENVIRONMENT_PARAM)
  const query = env ? `?${ENVIRONMENT_PARAM}=${encodeURIComponent(env)}` : ''
  return (
    <nav
      ref={navRef}
      aria-label={`${section.title} pages`}
      className="mb-6 flex min-h-10 w-full items-center gap-5 overflow-x-auto border-b text-muted-foreground"
    >
      {section.tabs.map((item) => {
        const active = item.url === tab.url
        return (
          <Link
            key={item.url}
            to={`${projectRoot(projectSlug)}/${item.url}${query}`}
            aria-current={active ? 'page' : undefined}
            className={cn(
              'inline-flex min-h-10 shrink-0 items-center gap-2 whitespace-nowrap border-b-2 border-transparent py-3 text-sm font-medium hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
              active && 'border-foreground text-foreground'
            )}
          >
            {/* Labels only on a phone, like `TabsTrigger`. */}
            <item.icon
              className="size-4 shrink-0 max-sm:hidden"
              aria-hidden="true"
            />
            {item.title}
          </Link>
        )
      })}
    </nav>
  )
}

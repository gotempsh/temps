// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Check, ChevronDown } from 'lucide-react'
import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentType,
  type ReactNode,
} from 'react'
import { Link } from 'react-router'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { cn } from '@/lib/utils'
import { fittingTabs } from './page-tab-fit'

/** One of the pages behind a tab that holds several. */
export interface PageTabPage {
  key: string
  title: string
  icon: ComponentType<{ className?: string }>
  to: string
  active: boolean
}

export interface PageTab {
  key: string
  title: string
  icon: ComponentType<{ className?: string }>
  to: string
  active: boolean
  featureKey?: string
  /** The quick tour's anchor for this tab. */
  tour?: string
  /**
   * The pages behind the tab, when it holds several. The tab then opens a
   * menu of them: a page never gets a second row of tabs.
   */
  pages?: PageTabPage[]
}

/** The width kept for "More" while deciding what fits (label and chevron). */
const MORE_WIDTH = 68

const TAB =
  'inline-flex min-h-10 shrink-0 items-center gap-2 whitespace-nowrap border-b-2 border-transparent py-3 text-sm font-medium hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring data-[state=open]:text-foreground'
const TAB_ACTIVE = 'border-foreground text-foreground'

interface Measured {
  /** The tabs these widths were read for. */
  signature: string
  widths: number[]
  trailing: number
  gap: number
}

/** What a tab reads: its title, and the page shown when it holds several. */
function shownPage(tab: PageTab): PageTabPage | undefined {
  const page = tab.pages?.find((candidate) => candidate.active)
  return page && page.title !== tab.title ? page : undefined
}

function PageLinks({ pages }: { pages: PageTabPage[] }) {
  return (
    <>
      {pages.map((page) => (
        <DropdownMenuItem key={page.key} asChild>
          <Link
            to={page.to}
            aria-current={page.active ? 'page' : undefined}
            className={cn('gap-2', page.active && 'font-medium')}
          >
            <page.icon
              className="size-4 shrink-0 text-muted-foreground"
              aria-hidden="true"
            />
            <span className="min-w-0 flex-1">{page.title}</span>
            {page.active && (
              <Check className="size-4 shrink-0" aria-hidden="true" />
            )}
          </Link>
        </DropdownMenuItem>
      ))}
    </>
  )
}

/**
 * An area's pages as one row of underline tabs above the content. They
 * navigate between URLs, so they are links in a `nav` (new tab, copy link
 * and history work), styled like the shared underline `Tabs`. One page
 * renders no strip.
 *
 * Every tab leads with an icon, and so does every entry of a menu. It is
 * always one row. A tab that holds several pages opens a menu of
 * them, where another design would stack a second strip. The tabs that do
 * not fit are listed under "More", so the strip neither scrolls sideways nor
 * wraps; the tab shown is never one of them.
 */
export function PageTabStrip({
  label,
  tabs,
  trailing,
  className,
}: {
  /** Names the strip for assistive technology, such as "Errors pages". */
  label: string
  tabs: PageTab[]
  /** A control after the last tab, such as what else can be added. */
  trailing?: ReactNode
  className?: string
}) {
  const navRef = useRef<HTMLElement>(null)
  const [measured, setMeasured] = useState<Measured>()
  const [available, setAvailable] = useState(0)
  const hasTrailing = Boolean(trailing)
  // What a tab reads decides its width, so a change of it measures again.
  const signature = [
    ...tabs.map((tab) => `${tab.key}:${shownPage(tab)?.key ?? ''}`),
    hasTrailing ? 'trailing' : '',
  ].join('|')
  const measuring = measured?.signature !== signature

  // Every tab is drawn while measuring; the strip clips, so nothing shows
  // out of place before the next paint.
  useLayoutEffect(() => {
    const nav = navRef.current
    if (!nav || !measuring) return
    const width = (element: Element) => element.getBoundingClientRect().width
    setMeasured({
      signature,
      widths: [...nav.querySelectorAll('[data-tab]')].map(width),
      trailing: [...nav.querySelectorAll('[data-trailing]')]
        .map(width)
        .reduce((sum, part) => sum + part, 0),
      gap: Number.parseFloat(window.getComputedStyle(nav).columnGap) || 0,
    })
    setAvailable(nav.clientWidth)
  }, [measuring, signature])

  const rendered = tabs.length >= 2 || hasTrailing
  useEffect(() => {
    const nav = navRef.current
    if (!nav || !rendered) return
    // A web font arriving changes every width read before it.
    let stale = false
    void document.fonts?.ready.then(() => {
      if (!stale) setMeasured(undefined)
    })
    if (typeof ResizeObserver === 'undefined')
      return () => {
        stale = true
      }
    const observer = new ResizeObserver(() => setAvailable(nav.clientWidth))
    observer.observe(nav)
    return () => {
      stale = true
      observer.disconnect()
    }
  }, [rendered])

  if (!rendered) return null

  const all = tabs.map((_, index) => index)
  const shown =
    measured && !measuring
      ? fittingTabs(measured.widths, {
          available,
          gap: measured.gap,
          more: MORE_WIDTH,
          trailing: measured.trailing,
          current: tabs.findIndex((tab) => tab.active),
        })
      : all
  const rest = all.filter((index) => !shown.includes(index))

  return (
    <nav
      ref={navRef}
      aria-label={label}
      className={cn(
        'mb-6 flex min-h-10 w-full items-center gap-5 overflow-hidden border-b text-muted-foreground',
        className
      )}
    >
      {shown.map((index) => {
        const tab = tabs[index]
        const badge = tab.featureKey && (
          <FeatureMaturityBadge featureKey={tab.featureKey} compact />
        )
        // Every tab leads with its icon, at every width: a strip is
        // scanned by shape before it is read.
        const icon = <tab.icon className="size-4 shrink-0" aria-hidden="true" />
        if (!tab.pages)
          return (
            <Link
              key={tab.key}
              to={tab.to}
              data-tab=""
              data-tour={tab.tour}
              aria-current={tab.active ? 'page' : undefined}
              className={cn(TAB, tab.active && TAB_ACTIVE)}
            >
              {icon}
              {tab.title}
              {badge}
            </Link>
          )
        const page = shownPage(tab)
        return (
          <DropdownMenu key={tab.key}>
            <DropdownMenuTrigger
              data-tab=""
              data-tour={tab.tour}
              aria-current={tab.active ? 'true' : undefined}
              className={cn(TAB, 'gap-1.5', tab.active && TAB_ACTIVE)}
            >
              {icon}
              {tab.title}
              {page && (
                <span className="font-normal text-muted-foreground">
                  {page.title}
                </span>
              )}
              {badge}
              <ChevronDown
                className="size-3.5 shrink-0 opacity-70"
                aria-hidden="true"
              />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="min-w-44">
              <PageLinks pages={tab.pages} />
            </DropdownMenuContent>
          </DropdownMenu>
        )
      })}
      {rest.length > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger
            aria-label={`More ${label}`}
            className={cn(TAB, 'gap-1.5')}
          >
            More
            <ChevronDown
              className="size-3.5 shrink-0 opacity-70"
              aria-hidden="true"
            />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-48">
            {rest.map((index, position) => {
              const tab = tabs[index]
              if (!tab.pages)
                return (
                  <DropdownMenuItem key={tab.key} asChild>
                    <Link to={tab.to} data-tour={tab.tour} className="gap-2">
                      <tab.icon
                        className="size-4 shrink-0 text-muted-foreground"
                        aria-hidden="true"
                      />
                      {tab.title}
                    </Link>
                  </DropdownMenuItem>
                )
              return (
                <Fragment key={tab.key}>
                  {position > 0 && <DropdownMenuSeparator />}
                  <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
                    {tab.title}
                  </DropdownMenuLabel>
                  <PageLinks pages={tab.pages} />
                  {position < rest.length - 1 &&
                    !tabs[rest[position + 1]].pages && (
                      <DropdownMenuSeparator />
                    )}
                </Fragment>
              )
            })}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {hasTrailing && (
        <span data-trailing="" className="inline-flex shrink-0">
          {trailing}
        </span>
      )}
    </nav>
  )
}

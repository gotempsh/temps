// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { Sparkles, X } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { useNavigate, useParams } from 'react-router'
import { useIsMobile } from '@/components/hooks/use-mobile'
import {
  getProjectTourNavigationTarget,
  getProjectTourCardStyle,
  isProjectTourHomePage,
  PROJECT_TOUR_EVENT,
  setProjectTourActive,
} from '@/lib/project-tour'

// Keep the existing module API for consumers while the shared state lives in a
// non-component module that remains safe for React Fast Refresh.
/* eslint-disable react-refresh/only-export-components */
export {
  PROJECT_TOUR_EVENT,
  isProjectTourActive,
  useProjectTourActive,
} from '@/lib/project-tour'
/* eslint-enable react-refresh/only-export-components */

/**
 * A lightweight, dependency-free guided tour for new projects. It walks the user
 * through the "sites of interest" by navigating to each page — Overview,
 * Analytics, Traces, Error tracking, Logs, Metrics — while a coachmark card and
 * highlight ring point at either the direct destination or the grouped project
 * tools menu. Auto-runs once per browser on the first project visit; re-launch by
 * dispatching a `temps:project-tour` event.
 */

const SEEN_KEY = 'temps.project-tour.v1'

interface TourStep {
  route: string
  anchor: string // data-tour of the sidebar item to anchor/point at
  title: string
  body: string
}

const STEPS: TourStep[] = [
  {
    route: 'project',
    anchor: 'project',
    title: 'Overview',
    body: 'Your project home — deployments, status and setup all live here.',
  },
  {
    route: 'analytics',
    anchor: 'all-tools',
    title: 'Analytics',
    body: 'Pageviews, visitors, funnels and session replays from your app.',
  },
  {
    route: 'traces',
    anchor: 'all-tools',
    title: 'Traces',
    body: 'Distributed OpenTelemetry traces — every request, span by span.',
  },
  {
    route: 'errors',
    anchor: 'errors',
    title: 'Error tracking',
    body: 'Exceptions with stack traces, grouped and alertable.',
  },
  {
    route: 'metrics',
    anchor: 'all-tools',
    title: 'Metrics',
    body: 'Counters, histograms and gauges — with anomaly alerts.',
  },
  {
    route: 'runtime',
    anchor: 'runtime',
    title: 'Runtime logs',
    body: 'Live logs streamed straight from your running containers.',
  },
]

export function ProjectTour() {
  const navigate = useNavigate()
  const { slug } = useParams<{ slug: string }>()
  const isMobile = useIsMobile()
  const [active, setActive] = useState(false)
  const [idx, setIdx] = useState(0)
  const [rect, setRect] = useState<DOMRect | null>(null)

  const start = useCallback(() => {
    setIdx(0)
    setActive(true)
    setProjectTourActive(true)
  }, [])

  const finish = useCallback(() => {
    setActive(false)
    setProjectTourActive(false)
    try {
      window.localStorage.setItem(SEEN_KEY, '1')
    } catch {
      // storage disabled — the tour simply runs again next time
    }
  }, [])

  // Auto-start once per browser, plus a manual re-launch via window event.
  useEffect(() => {
    const onStart = () => start()
    window.addEventListener(PROJECT_TOUR_EVENT, onStart)

    const seen = (() => {
      try {
        return !!window.localStorage.getItem(SEEN_KEY)
      } catch {
        return true
      }
    })()
    // Only auto-start from the project's home page. A deep link straight into a
    // specific sub-page — a shared deployment URL, a bookmark, browser back —
    // must never be hijacked by the tour's own forced navigation to "project".
    const onHomePage = isProjectTourHomePage(slug, window.location.pathname)
    const timer =
      seen || !onHomePage ? undefined : window.setTimeout(start, 800)

    return () => {
      window.removeEventListener(PROJECT_TOUR_EVENT, onStart)
      if (timer) window.clearTimeout(timer)
    }
  }, [slug, start])

  // Navigate to each step's page as the tour advances, so the user sees it.
  //
  // Keyed on the step itself, not on every run of the effect: `navigate` gets a
  // new identity whenever the location changes, so re-running turned any step
  // whose route redirects into an infinite loop. Metrics is one — `…/metrics`
  // immediately redirects to `…/metrics/explore`, which changed the location,
  // which re-ran this effect, which pushed `…/metrics` again (hundreds of
  // history entries, a flickering URL bar and a destroyed Back button).
  const navigatedFor = useRef<string | null>(null)
  useEffect(() => {
    if (!active || !slug) {
      navigatedFor.current = null
      return
    }
    const target = getProjectTourNavigationTarget({
      active,
      slug,
      route: STEPS[idx].route,
      lastTarget: navigatedFor.current,
    })
    if (!target) return
    navigatedFor.current = target
    navigate(target)
  }, [active, idx, slug, navigate])

  // Measure the anchor sidebar item to place the card + ring (deferred a frame,
  // and kept aligned on scroll/resize).
  useEffect(() => {
    if (!active) return
    const measure = () => {
      const el = document.querySelector<HTMLElement>(
        `[data-tour="${STEPS[idx].anchor}"]`
      )
      setRect(el ? el.getBoundingClientRect() : null)
    }
    const raf = requestAnimationFrame(measure)
    window.addEventListener('resize', measure)
    window.addEventListener('scroll', measure, true)
    return () => {
      cancelAnimationFrame(raf)
      window.removeEventListener('resize', measure)
      window.removeEventListener('scroll', measure, true)
    }
  }, [active, idx])

  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') finish()
      if (e.key === 'Enter') setIdx((i) => (i >= STEPS.length - 1 ? i : i + 1))
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [active, finish])

  if (!active) return null

  const step = STEPS[idx]
  const isLast = idx === STEPS.length - 1

  const cardStyle = getProjectTourCardStyle({
    isMobile,
    anchor: rect,
    viewportWidth: window.innerWidth,
    viewportHeight: window.innerHeight,
  })

  return createPortal(
    <>
      {rect && !isMobile && (
        <div
          className="pointer-events-none fixed z-[95] rounded-md ring-2 ring-primary ring-offset-2 ring-offset-background transition-all"
          style={{
            top: rect.top - 2,
            left: rect.left - 2,
            width: rect.width + 4,
            height: rect.height + 4,
          }}
        />
      )}
      <div
        data-testid="project-tour-card"
        className="fixed z-[100] w-auto rounded-xl border bg-popover p-4 text-popover-foreground shadow-2xl md:w-80"
        style={cardStyle}
      >
        <div className="flex items-center justify-between">
          <span className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <Sparkles className="size-3.5 text-primary" />
            Quick tour · {idx + 1}/{STEPS.length}
          </span>
          <button
            type="button"
            onClick={finish}
            aria-label="Close tour"
            className="text-muted-foreground transition-colors hover:text-foreground"
          >
            <X className="size-4" />
          </button>
        </div>
        <p className="mt-2 text-sm font-semibold">{step.title}</p>
        <p className="mt-1 text-sm text-muted-foreground">{step.body}</p>
        <div className="mt-4 flex items-center justify-between">
          <div className="flex gap-1">
            {STEPS.map((_, i) => (
              <span
                key={i}
                className={cn(
                  'size-1.5 rounded-full',
                  i === idx ? 'bg-primary' : 'bg-muted'
                )}
              />
            ))}
          </div>
          <div className="flex gap-2">
            {idx > 0 && (
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setIdx((i) => Math.max(0, i - 1))}
              >
                Back
              </Button>
            )}
            <Button
              size="sm"
              onClick={() => (isLast ? finish() : setIdx((i) => i + 1))}
            >
              {isLast ? 'Done' : 'Next'}
            </Button>
          </div>
        </div>
      </div>
    </>,
    document.body
  )
}

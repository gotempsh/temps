// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { emailSetupSteps, type EmailSetupStep } from '@/lib/email-setup'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { Check } from 'lucide-react'
import { Link } from 'react-router'
import {
  fetchEmailDomains,
  fetchEmailProviders,
  fetchEmailStats,
  fetchEventStats,
} from './email-queries'

function SetupStep({
  step,
  number,
  current,
}: {
  step: EmailSetupStep
  number: number
  current: boolean
}) {
  return (
    <li className="flex items-start gap-3 p-4">
      <span
        className={cn(
          'flex size-6 shrink-0 items-center justify-center rounded-full border text-xs font-medium tabular-nums',
          step.done && 'border-transparent bg-foreground text-background',
          !step.done && !current && 'text-muted-foreground'
        )}
      >
        {step.done ? <Check className="size-3.5" aria-label="Done" /> : number}
      </span>
      <div className="min-w-0 flex-1 space-y-1">
        <p
          className={cn(
            'text-sm font-medium',
            !step.done && !current && 'text-muted-foreground'
          )}
        >
          {step.title}
        </p>
        <p className="text-sm text-muted-foreground">{step.detail}</p>
        {current && step.action && (
          <Button asChild size="sm" className="mt-2">
            <Link to={step.action.to}>{step.action.label}</Link>
          </Button>
        )}
      </div>
    </li>
  )
}

/**
 * The way from nothing to a delivered email. Shows what is done and asks for
 * one thing at a time; it leaves once there is nothing left to do. It waits for
 * real answers, so a failed lookup never claims a step is still to do.
 */
export function EmailGetStarted() {
  const providers = useQuery({
    queryKey: ['email-providers'],
    queryFn: fetchEmailProviders,
  })
  const domains = useQuery({
    queryKey: ['email-domains'],
    queryFn: fetchEmailDomains,
  })
  const stats = useQuery({
    queryKey: ['email-stats', undefined],
    queryFn: () => fetchEmailStats(),
  })

  if (!providers.data || !domains.data || !stats.data) return null

  const steps = emailSetupSteps({
    providers: providers.data,
    domains: domains.data,
    stats: stats.data,
  })
  const current = steps.findIndex((step) => !step.done)
  if (current === -1) return null

  return (
    <section
      aria-labelledby="email-setup-heading"
      className="rounded-lg border"
    >
      <div className="flex items-baseline justify-between gap-3 border-b p-4">
        <h2 id="email-setup-heading" className="text-base font-semibold">
          Set up sending
        </h2>
        <span className="shrink-0 text-sm text-muted-foreground">
          {current} of {steps.length} done
        </span>
      </div>
      <ol className="divide-y">
        {steps.map((step, index) => (
          <SetupStep
            key={step.id}
            step={step}
            number={index + 1}
            current={index === current}
          />
        ))}
      </ol>
    </section>
  )
}

function Tile({
  label,
  value,
  rate,
}: {
  label: string
  value: number
  rate?: number | null
}) {
  return (
    <div className="min-w-0 space-y-1 bg-background p-4">
      <dt className="truncate text-sm text-muted-foreground">{label}</dt>
      <dd className="flex items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">
          {value.toLocaleString()}
        </span>
        {rate != null && (
          <span className="text-sm text-muted-foreground tabular-nums">
            {Math.round(rate)}%
          </span>
        )}
      </dd>
    </div>
  )
}

/**
 * Delivery and engagement in one strip. Events span every email on the
 * instance, so only an administrator can read them; everyone else gets the
 * delivery counts. Nothing shows until something has been sent: a row of zeros
 * tells someone setting up nothing the checklist has not already said.
 */
export function EmailStatsStrip() {
  const isAdmin = useIsInstanceAdmin()
  const stats = useQuery({
    queryKey: ['email-stats', undefined],
    queryFn: () => fetchEmailStats(),
  })
  const events = useQuery({
    queryKey: ['email-event-stats'],
    queryFn: fetchEventStats,
    enabled: isAdmin,
  })

  if (stats.isLoading) return <Skeleton className="h-24 w-full rounded-lg" />
  if (!stats.data || stats.data.total === 0) return null

  const { sent, failed, total, queued, sending, delivery_unknown, captured } =
    stats.data
  const engagement = events.data
  const notes = [
    queued > 0 && `${queued.toLocaleString()} queued`,
    sending > 0 && `${sending.toLocaleString()} sending`,
    delivery_unknown > 0 &&
      `${delivery_unknown.toLocaleString()} delivery unknown`,
    captured > 0 && `${captured.toLocaleString()} captured, not delivered`,
  ].filter((note): note is string => Boolean(note))

  return (
    <section
      aria-label="Email totals, all time"
      className="@container space-y-2"
    >
      <dl
        className={cn(
          'grid gap-px overflow-hidden rounded-lg border bg-border',
          engagement
            ? 'grid-cols-2 @xl:grid-cols-3 @4xl:grid-cols-6'
            : 'grid-cols-3'
        )}
      >
        {engagement ? (
          <>
            <Tile label="Sent" value={sent} />
            <Tile
              label="Opened"
              value={engagement.opened}
              rate={engagement.open_rate}
            />
            <Tile
              label="Clicked"
              value={engagement.clicked}
              rate={engagement.click_rate}
            />
            <Tile
              label="Bounced"
              value={engagement.bounced}
              rate={engagement.bounce_rate}
            />
            <Tile label="Complained" value={engagement.complained} />
            <Tile label="Failed" value={failed} />
          </>
        ) : (
          <>
            <Tile label="Total" value={total} />
            <Tile label="Sent" value={sent} />
            <Tile label="Failed" value={failed} />
          </>
        )}
      </dl>
      <p className="text-xs text-muted-foreground">
        {['All time', ...notes].join(' · ')}
      </p>
    </section>
  )
}

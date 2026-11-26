// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineRegistry } from '@json-render/react'
import { Callout, Status, StatusDot } from '@temps-sdk/ds'
import { ArrowRight } from 'lucide-react'
import { Link as RouterLink } from 'react-router'
import { Button } from '@/components/ui/button'
import { isInternalConsolePath } from '@/lib/cloud-telemetry-activation'
import { viewCatalog } from './catalog'
import { ConsoleLink } from './ConsoleLink'
import { LazyViewTrend } from './LazyViewTrend'
import { ViewBars } from './ViewBars'
import { ViewTable } from './ViewTable'

/**
 * How each catalog component is drawn: with the console's own primitives, so
 * a view the assistant writes reads like the page it links to.
 */
export const { registry: viewRegistry } = defineRegistry(viewCatalog, {
  components: {
    Stack: ({ children }) => (
      <div className="flex min-w-0 flex-col gap-3">{children}</div>
    ),
    // As many tiles in a row as fit the chat's own width, never fewer than
    // two on a phone: no breakpoint knows how wide the chat column is.
    Stats: ({ children }) => (
      <div className="grid grid-cols-[repeat(auto-fit,minmax(8rem,1fr))] gap-2">
        {children}
      </div>
    ),
    Stat: ({ props }) => (
      <div className="min-w-0 rounded-md border bg-card px-3 py-2">
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          {props.tone ? <StatusDot tone={props.tone} /> : null}
          <span className="truncate">{props.label}</span>
        </p>
        <p className="text-lg font-semibold [overflow-wrap:anywhere]">
          {props.value}
        </p>
        {props.detail ? (
          <p className="text-xs text-muted-foreground">{props.detail}</p>
        ) : null}
      </div>
    ),
    Status: ({ props }) => (
      <div>
        <Status tone={props.tone} label={props.label} />
      </div>
    ),
    KeyValues: ({ props }) => (
      <dl className="divide-y rounded-md border text-sm">
        {props.items.map((item, index) => (
          <div
            key={`${index}:${item.label}`}
            className="flex min-w-0 items-baseline justify-between gap-4 px-3 py-2"
          >
            <dt className="shrink-0 text-muted-foreground">{item.label}</dt>
            <dd className="min-w-0 text-right [overflow-wrap:anywhere]">
              <ConsoleLink href={item.href}>{item.value}</ConsoleLink>
            </dd>
          </div>
        ))}
      </dl>
    ),
    Table: ({ props }) => (
      <ViewTable columns={props.columns} rows={props.rows} />
    ),
    Trend: ({ props }) => (
      <LazyViewTrend
        label={props.label}
        points={props.points}
        partial={props.partial}
      />
    ),
    Bars: ({ props }) => <ViewBars items={props.items} />,
    Callout: ({ props }) => (
      <Callout tone={props.tone} title={props.title}>
        {props.text}
      </Callout>
    ),
    Link: ({ props }) =>
      isInternalConsolePath(props.href) ? (
        <div>
          <Button asChild variant="outline" size="sm">
            <RouterLink to={props.href}>
              {props.label}
              <ArrowRight className="size-4" aria-hidden="true" />
            </RouterLink>
          </Button>
        </div>
      ) : null,
  },
})

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Fragment, createElement, type ReactNode } from 'react'
import { Bot, MapPin } from 'lucide-react'
import type { VisitorInfo } from '@/api/client/types.gen'
import { channelIcon } from '@/components/analytics/overview/channel-identity'
import { ReferrerIcon } from '@/components/analytics/overview/ReferrerIdentity'
import { OsIcon } from '@/components/analytics/overview/TechnologyIcon'
import { BrowserLogo } from '@/components/ui/browser-logo'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  locationSummary,
  sourceSummary,
  technologySummary,
  type IdentityFilter,
  type IdentityFilterKey,
} from './visitor-identity'

export interface IdentityFilterProps {
  /** The value the list is currently filtered by for `key`, if any. */
  activeFilter: (key: IdentityFilterKey) => string | undefined
  onFilter: (key: IdentityFilterKey, value: string | undefined) => void
}

/** A fixed 16px slot, so text lines up whichever icon (or none) loads. */
function IconSlot({ children }: { children: ReactNode }) {
  return (
    <span
      aria-hidden="true"
      className="inline-flex size-4 shrink-0 items-center justify-center overflow-hidden"
    >
      {children}
    </span>
  )
}

/**
 * For the part that should stay readable longest (the country, the channel):
 * it shrinks only once the other part has given way, so a very long value
 * still truncates instead of overflowing the line.
 */
const GIVES_WAY_LAST = 'shrink-[0.001]'

/** One line that truncates, with everything it knows in a tooltip. */
function IdentityLine({
  detail,
  children,
  className = 'max-w-56',
}: {
  detail?: string
  children: ReactNode
  className?: string
}) {
  const classes = `flex min-w-0 items-center gap-2 whitespace-nowrap ${className}`
  if (!detail) return <span className={classes}>{children}</span>
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        {/* A tab stop of its own, so keyboard users reach the tooltip's
            details; filter buttons inside stay separate tab stops. */}
        <span
          tabIndex={0}
          className={`${classes} rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring`}
        >
          {children}
        </span>
      </TooltipTrigger>
      <TooltipContent className="max-w-sm [overflow-wrap:anywhere]">
        {detail}
      </TooltipContent>
    </Tooltip>
  )
}

/**
 * A value that filters the list by itself when clicked. It truncates on its
 * own text, so a long value ends in an ellipsis instead of disappearing.
 */
function FilterText({
  label,
  filter,
  activeFilter,
  onFilter,
  className = '',
}: {
  label: string
  filter?: IdentityFilter
  className?: string
} & IdentityFilterProps) {
  if (!filter)
    return <span className={`min-w-0 truncate ${className}`}>{label}</span>
  const active = activeFilter(filter.key) === filter.value
  return (
    <button
      type="button"
      className={`min-w-0 truncate rounded-sm text-left decoration-muted-foreground underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring aria-pressed:font-medium ${className}`}
      aria-label={`Filter by ${label}`}
      aria-pressed={active}
      onClick={() => onFilter(filter.key, active ? undefined : filter.value)}
    >
      {label}
    </button>
  )
}

export function VisitorLocationCell({
  visitor,
  ...filters
}: { visitor: VisitorInfo } & IdentityFilterProps) {
  const location = locationSummary(visitor)
  return (
    <IdentityLine detail={location.detail}>
      <IconSlot>
        {location.flag ? (
          <span className="text-sm leading-none">{location.flag}</span>
        ) : (
          <MapPin className="size-4 text-muted-foreground" />
        )}
      </IconSlot>
      {location.parts.length === 0 ? (
        <span className="text-muted-foreground">Unknown</span>
      ) : (
        <span className="flex min-w-0 items-center">
          {location.parts.map((part, index) => (
            <Fragment key={part.label}>
              {index > 0 && <span className="shrink-0">,&nbsp;</span>}
              <FilterText
                {...part}
                {...filters}
                // The city gives way first, so the country stays readable.
                className={
                  index === location.parts.length - 1 ? GIVES_WAY_LAST : ''
                }
              />
            </Fragment>
          ))}
        </span>
      )}
    </IdentityLine>
  )
}

export function VisitorSourceCell({
  visitor,
  ...filters
}: { visitor: VisitorInfo } & IdentityFilterProps) {
  const source = sourceSummary(visitor)
  return (
    <IdentityLine detail={source.detail} className="max-w-72">
      <IconSlot>
        {source.domain ? (
          <ReferrerIcon
            key={source.domain}
            domain={source.domain}
            className="size-4"
          />
        ) : (
          createElement(channelIcon(source.channel ?? 'Direct'), {
            className: 'size-4 text-muted-foreground',
          })
        )}
      </IconSlot>
      <span className="flex min-w-0 items-center">
        <FilterText
          {...source.primary}
          {...filters}
          className={source.secondary ? GIVES_WAY_LAST : ''}
        />
        {source.secondary && (
          <>
            <span className="shrink-0 text-muted-foreground">
              &nbsp;·&nbsp;
            </span>
            <FilterText
              {...source.secondary}
              {...filters}
              className="text-muted-foreground"
            />
          </>
        )}
      </span>
    </IdentityLine>
  )
}

export function VisitorTechnologyCell({ visitor }: { visitor: VisitorInfo }) {
  const technology = technologySummary(visitor)
  return (
    <IdentityLine detail={technology.detail}>
      {technology.browser && (
        <span className="flex min-w-0 items-center gap-1.5">
          <IconSlot>
            <BrowserLogo browser={technology.browser} size={16} />
          </IconSlot>
          <span className="truncate">{technology.browser}</span>
        </span>
      )}
      {technology.os && (
        <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
          <IconSlot>
            <OsIcon os={technology.os} size={16} />
          </IconSlot>
          <span className="truncate">{technology.os}</span>
        </span>
      )}
      {!technology.browser &&
        !technology.os &&
        (visitor.is_crawler ? (
          <span className="flex min-w-0 items-center gap-1.5 text-muted-foreground">
            <IconSlot>
              <Bot className="size-4" />
            </IconSlot>
            Crawler
          </span>
        ) : (
          <span className="text-muted-foreground">Unknown</span>
        ))}
    </IdentityLine>
  )
}

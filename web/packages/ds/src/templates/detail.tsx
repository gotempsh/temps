// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { PageContainer, PageHeader } from '../page-header'
import { cn } from '../lib/cn'

export interface DetailFact {
  label: ReactNode
  value: ReactNode
}

export interface DetailProps {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  /** The record recipe's second beat — pass a `<Status ... />`. */
  verdict?: ReactNode
  /**
   * The record recipe's third beat: 4-6 scannable facts, rendered as a
   * definition-list grid directly under the header. More than 6 and the
   * page has stopped triaging and started dumping — move the rest into
   * `main` or `aside`.
   */
  facts: DetailFact[]
  /** The record's primary content — logs, config, the main narrative. */
  main: ReactNode
  /** Secondary content: related resources, metadata, a timeline. */
  aside?: ReactNode
  className?: string
}

/**
 * The record template: title -> verdict -> facts -> main column + aside.
 * See RULES.md § "Record page checklist" before reaching for this directly —
 * it enforces the shape but not the judgment call of what counts as a fact
 * vs. main content.
 */
export function Detail({
  title,
  description,
  actions,
  verdict,
  facts,
  main,
  aside,
  className,
}: DetailProps) {
  return (
    <PageContainer className={className}>
      <PageHeader title={title} description={description} verdict={verdict} actions={actions} />
      {facts.length > 0 ? (
        <dl
          className={cn(
            'grid grid-cols-2 gap-x-6 gap-y-3 rounded-md border bg-muted/20 p-4 text-sm sm:grid-cols-3 lg:grid-cols-6',
          )}
        >
          {facts.map((fact, i) => (
            <div key={i} className="min-w-0 space-y-0.5">
              <dt className="text-xs text-muted-foreground">{fact.label}</dt>
              <dd className="truncate font-medium">{fact.value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-3">
        <div className="min-w-0 space-y-6 lg:col-span-2">{main}</div>
        {aside ? <div className="min-w-0 space-y-6">{aside}</div> : null}
      </div>
    </PageContainer>
  )
}

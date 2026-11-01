// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'

/**
 * One section of the Shield tab, inside the Activity or Tools group: a
 * heading, a short description, optional actions, and its single surface.
 * A section shown on its own (every event) heads the page: `level={2}`.
 */
export function ShieldSection({
  id,
  title,
  description,
  actions,
  level = 3,
  children,
}: {
  id: string
  title: string
  description?: string
  actions?: ReactNode
  level?: 2 | 3
  children: ReactNode
}) {
  const headingId = `${id}-heading`
  const Heading = level === 2 ? 'h2' : 'h3'
  return (
    <section aria-labelledby={headingId} className="min-w-0 space-y-3">
      <div className="flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0 space-y-1">
          <Heading id={headingId} className="text-base font-semibold">
            {title}
          </Heading>
          {description && (
            <p className="max-w-3xl text-sm text-muted-foreground">
              {description}
            </p>
          )}
        </div>
        {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
      </div>
      <div className="min-w-0 rounded-lg border bg-card text-card-foreground">
        {children}
      </div>
    </section>
  )
}

/**
 * A top-level group of the Shield tab (Activity, Tools): a heading the
 * sections below it sit under, without a surface of its own.
 */
export function ShieldGroup({
  id,
  title,
  description,
  actions,
  children,
}: {
  id: string
  title: string
  description?: string
  actions?: ReactNode
  children: ReactNode
}) {
  const headingId = `${id}-heading`
  return (
    <section aria-labelledby={headingId} className="min-w-0 space-y-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
        <div className="min-w-0 space-y-1">
          <h2 id={headingId} className="text-lg font-semibold">
            {title}
          </h2>
          {description && (
            <p className="max-w-3xl text-sm text-muted-foreground">
              {description}
            </p>
          )}
        </div>
        {actions && <div className="flex flex-wrap gap-2">{actions}</div>}
      </div>
      {children}
    </section>
  )
}

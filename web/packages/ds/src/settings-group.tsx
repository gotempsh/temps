// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'

/** Open settings group: section identity left, controls right; stacked on mobile. */
export function SettingsGroup({
  title,
  description,
  children,
  headingLevel: Heading = 'h2',
  id,
}: {
  headingLevel?: 'h2' | 'h3'
  /** DOM id, so a link can land on this group (`…#crash-autofix`). */
  id?: string
  title: string
  /** Optional scope or outcome; omit when the title is sufficient. */
  description?: string
  children: ReactNode
}) {
  const headingId = useId()
  return (
    <section
      id={id}
      aria-labelledby={headingId}
      className="grid min-w-0 gap-5 md:grid-cols-[minmax(0,1fr)_minmax(0,2fr)] md:gap-10"
    >
      <div className="min-w-0">
        <Heading id={headingId} className="text-base font-semibold">
          {title}
        </Heading>
        {description && (
          <p className="mt-1 text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      <div className="min-w-0 space-y-5">{children}</div>
    </section>
  )
}

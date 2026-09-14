// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'
import { Card, CardContent, CardHeader } from '@/components/ui/card'

/** Consistent heading, controls and content spacing for charts and breakdowns. */
export function DataSection({
  title,
  description,
  actions,
  children,
}: {
  title: ReactNode
  description?: ReactNode
  actions?: ReactNode
  children: ReactNode
}) {
  const id = useId()
  return (
    <section aria-labelledby={id} className="min-w-0">
      <Card>
        <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-3 border-b px-5 py-4">
          <div className="min-w-0">
            <h2 id={id} className="text-base font-semibold">
              {title}
            </h2>
            {description && (
              <p className="mt-1 text-sm text-muted-foreground">
                {description}
              </p>
            )}
          </div>
          {actions && <div className="min-w-0 max-w-full">{actions}</div>}
        </CardHeader>
        <CardContent className="p-5">{children}</CardContent>
      </Card>
    </section>
  )
}

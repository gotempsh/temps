// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@temps-sdk/ds'
import { Skeleton } from '@/components/ui/skeleton'

/**
 * What a settings form shows while the platform settings are being read: the
 * page heading (when it has one) over placeholder fields shaped like the
 * form, so the page neither reads as blank nor jumps when the values arrive.
 */
export function SettingsFormSkeleton({
  title,
  label,
}: {
  /** The page's own heading, kept visible while loading. */
  title?: string
  /** Accessible name of the loading region, e.g. "Loading build limits". */
  label: string
}) {
  return (
    <div
      role="status"
      aria-busy="true"
      aria-label={label}
      className="w-full min-w-0 space-y-6"
    >
      {title && <PageHeader title={title} />}
      {[0, 1].map((section) => (
        <div key={section} className="space-y-4 rounded-lg border p-4">
          <Skeleton className="h-5 w-40" />
          <Skeleton className="h-4 w-2/3" />
          <div className="space-y-2">
            <Skeleton className="h-4 w-32" />
            <Skeleton className="h-9 w-full" />
          </div>
          <div className="space-y-2">
            <Skeleton className="h-4 w-24" />
            <Skeleton className="h-9 w-full" />
          </div>
        </div>
      ))}
    </div>
  )
}

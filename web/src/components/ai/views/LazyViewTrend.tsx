// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { lazy, Suspense, type ComponentProps } from 'react'
import { Skeleton } from '@/components/ui/skeleton'

const ViewTrend = lazy(() => import('./ViewTrend'))

/**
 * The trend chart, loaded when an answer first draws one rather than with
 * the chat: the chart library is large and most answers have no trend.
 */
export function LazyViewTrend(props: ComponentProps<typeof ViewTrend>) {
  return (
    <Suspense fallback={<Skeleton className="h-[250px] w-full" />}>
      <ViewTrend {...props} />
    </Suspense>
  )
}

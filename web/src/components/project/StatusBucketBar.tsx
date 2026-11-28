// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { StatusBucket } from '@/api/client'
import {
  BUCKET_TONE_CLASS,
  bucketParts,
  type BucketPart,
} from '@/lib/monitor-uptime'
import { cn } from '@/lib/utils'

/**
 * One bucket of a monitor's timeline as a column split by what its checks
 * found: successful at the bottom, then degraded, then failed, each as tall
 * as its share of the checks. A part with any checks keeps a visible sliver,
 * so a recovery inside a mostly degraded hour still shows green. A bucket
 * nothing checked is one grey block.
 */
export function StatusBucketBar({
  bucket,
  compact = false,
  className,
}: {
  bucket: Pick<
    StatusBucket,
    'operational_count' | 'degraded_count' | 'down_count'
  >
  /** The monitor list's small timeline: thinner gaps and slivers. */
  compact?: boolean
  className?: string
}) {
  const parts = bucketParts(bucket)
  return (
    <div
      aria-hidden="true"
      className={cn(
        'flex h-full w-full flex-col-reverse overflow-hidden rounded-sm',
        compact ? 'gap-px' : 'gap-0.5',
        className
      )}
    >
      {parts.length === 0 ? (
        <div className={cn('flex-1', BUCKET_TONE_CLASS.unknown)} />
      ) : (
        parts.map((part) => (
          <Part key={part.tone} part={part} compact={compact} />
        ))
      )}
    </div>
  )
}

function Part({ part, compact }: { part: BucketPart; compact: boolean }) {
  return (
    <div
      data-tone={part.tone}
      className={BUCKET_TONE_CLASS[part.tone]}
      style={{
        flexGrow: part.count,
        flexShrink: 1,
        flexBasis: 0,
        minHeight: compact ? 2 : 3,
      }}
    />
  )
}

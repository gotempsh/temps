// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

interface RuntimeLogRowLayoutProps {
  metadata: ReactNode
  messageHtml: string
  className?: string
}

export const RUNTIME_LOG_ROW_CLASS =
  'flex flex-col gap-0.5 py-1 px-2 font-mono text-xs hover:bg-muted/50 sm:flex-row sm:items-start sm:gap-2 sm:py-0.5'
export const RUNTIME_LOG_METADATA_CLASS =
  'flex min-w-0 max-w-full flex-wrap items-center gap-x-2 gap-y-0.5 sm:contents'
export const RUNTIME_LOG_MESSAGE_CLASS =
  'min-w-0 w-full whitespace-pre-wrap break-words sm:w-auto sm:flex-1'

export function RuntimeLogRowLayout({
  metadata,
  messageHtml,
  className,
}: RuntimeLogRowLayoutProps) {
  return (
    <div className={cn(RUNTIME_LOG_ROW_CLASS, className)}>
      <div className={RUNTIME_LOG_METADATA_CLASS}>{metadata}</div>
      <span
        className={RUNTIME_LOG_MESSAGE_CLASS}
        dangerouslySetInnerHTML={{ __html: messageHtml }}
      />
    </div>
  )
}

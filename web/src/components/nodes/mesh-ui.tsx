// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Small pieces shared by the Worker Nodes mesh UI: the join guide, adding a
// server over SSH and the mesh hub card.

import { AlertTriangle, Loader2, RefreshCw } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { problemDetail } from '@/lib/api-problem'

/** Badge colors for the `tone` the wireguard-mesh helpers return. */
export const TONE_CLASSES = {
  ok: 'bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20',
  warn: 'bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/20',
  error: 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/20',
  muted: 'bg-gray-500/15 text-gray-700 dark:text-gray-400 border-gray-500/20',
} as const

/** Server-written text with `commands` in backticks, rendered as code. */
export function WithCode({ text }: { text: string }) {
  return (
    <>
      {text.split('`').map((part, index) =>
        index % 2 === 1 ? (
          <code key={index} className="rounded bg-muted px-1 font-mono">
            {part}
          </code>
        ) : (
          part
        )
      )}
    </>
  )
}

/** A query that failed: what could not be read, why, and a way to retry. */
export function QueryErrorAlert({
  title,
  error,
  onRetry,
  retrying,
}: {
  title: string
  error: unknown
  onRetry: () => void
  retrying?: boolean
}) {
  return (
    <Alert variant="destructive">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{problemDetail(error, 'The server did not answer.')}</p>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={onRetry}
          disabled={retrying}
        >
          {retrying ? (
            <Loader2 className="mr-1 h-4 w-4 animate-spin" />
          ) : (
            <RefreshCw className="mr-1 h-4 w-4" />
          )}
          Retry
        </Button>
      </AlertDescription>
    </Alert>
  )
}

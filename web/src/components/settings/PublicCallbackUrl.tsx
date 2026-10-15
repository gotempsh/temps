// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { ScrollRegion } from '@/components/ui/scroll-region'
import { Skeleton } from '@/components/ui/skeleton'
import { usePublicIngress } from '@/hooks/usePublicIngress'
import { cn } from '@/lib/utils'

interface PublicCallbackUrlProps {
  /** Absolute path appended to the public ingress URL, e.g. `/api/webhook/git/github/events`. */
  path: string
  /** Text shown before the URL but not copied, e.g. `POST`. */
  prefix?: string
  /** What the URL is, for the copy button's accessible label. */
  label?: string
  className?: string
  codeClassName?: string
}

/**
 * A callback URL a third party will call, with a copy button.
 *
 * The URL is built from the public ingress URL the API reports (ADR-051).
 * Until that is known there is nothing to copy: a skeleton while it loads,
 * and an explanation with a retry when the read fails. It never shows the
 * address being browsed instead — that is the console, which may be private,
 * and a provider configured with it would silently never reach Temps.
 */
export function PublicCallbackUrl({
  path,
  prefix,
  label = 'Copy URL',
  className,
  codeClassName,
}: PublicCallbackUrlProps) {
  const { callbackUrl, isPending, isError, isRefetching, refetch } =
    usePublicIngress()
  const url = callbackUrl(path)

  if (url === null && isPending) {
    return (
      <div className={cn('flex min-w-0 items-center gap-2', className)}>
        <Skeleton className="h-6 min-w-0 flex-1" />
      </div>
    )
  }

  if (url === null) {
    return (
      <div
        role="alert"
        className={cn('flex min-w-0 flex-wrap items-center gap-2', className)}
      >
        <p className="min-w-0 flex-1 text-xs text-destructive">
          {isError
            ? "Couldn't load the public callback URL, so there is nothing safe to copy yet."
            : 'No public callback URL is available.'}
        </p>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={isRefetching}
          onClick={() => void refetch()}
        >
          <RefreshCw
            className={cn('mr-2 h-3 w-3', isRefetching && 'animate-spin')}
          />
          Retry
        </Button>
      </div>
    )
  }

  return (
    <div className={cn('flex min-w-0 items-center gap-2', className)}>
      {/* codeClassName lands on the element that owns the background and
          padding, so callers can still replace them (`bg-transparent p-0`). */}
      <ScrollRegion
        className={cn(
          'min-w-0 flex-1 rounded bg-muted px-2 py-1 text-xs',
          codeClassName
        )}
        regionLabel={`${prefix ? `${prefix} ` : ''}callback URL, scrollable`}
      >
        <code className="block w-max whitespace-nowrap">
          {prefix ? `${prefix} ` : ''}
          {url}
        </code>
      </ScrollRegion>
      <CopyButton value={url} label={label} />
    </div>
  )
}

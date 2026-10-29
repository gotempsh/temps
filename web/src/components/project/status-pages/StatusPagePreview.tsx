// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { previewStatusPageOptions } from '@/api/client/@tanstack/react-query.gen'
import type { StatusPageResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useQuery } from '@tanstack/react-query'
import { Monitor, RefreshCw, Smartphone } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'

/**
 * What visitors see, rendered by the server exactly as the public page is
 * (drafts included). Shown in a sandboxed frame with no permissions: the
 * HTML has no scripts, and the sandbox keeps its links from navigating the
 * console.
 */
export function StatusPagePreview({ page }: { page: StatusPageResponse }) {
  const [narrow, setNarrow] = useState(false)
  const preview = useQuery({
    ...previewStatusPageOptions({ path: { status_page_id: page.id } }),
    // The public page refreshes every minute; so does the preview.
    refetchInterval: 60_000,
  })

  return (
    <section className="space-y-3">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h3 className="text-lg font-semibold">Preview</h3>
          <p className="text-sm text-muted-foreground">
            {page.is_published
              ? 'This is what visitors see right now. Hover a bar for that day’s uptime and response time.'
              : 'This is what visitors will see once you publish. Nothing is public yet.'}
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-pressed={narrow}
            onClick={() => setNarrow((v) => !v)}
          >
            {narrow ? (
              <Monitor className="size-4 sm:mr-2" />
            ) : (
              <Smartphone className="size-4 sm:mr-2" />
            )}
            <span className="max-sm:sr-only">
              {narrow ? 'Desktop width' : 'Mobile width'}
            </span>
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={preview.isFetching}
            onClick={() => preview.refetch()}
          >
            <RefreshCw
              className={cn(
                'size-4 sm:mr-2',
                preview.isFetching && 'animate-spin'
              )}
            />
            <span className="max-sm:sr-only">Refresh</span>
          </Button>
        </div>
      </div>
      {preview.isError ? (
        <ErrorAlert
          title="Failed to build the preview"
          description={
            preview.error instanceof Error
              ? preview.error.message
              : 'An unexpected error occurred'
          }
          retry={() => preview.refetch()}
        />
      ) : preview.isLoading || !preview.data ? (
        <Skeleton className="h-[640px] w-full rounded-lg" />
      ) : (
        <div className="overflow-hidden rounded-lg border bg-muted/30">
          <iframe
            title={`Preview of ${page.title}`}
            sandbox=""
            srcDoc={preview.data.html}
            className={cn(
              'mx-auto block h-[720px] w-full border-0 bg-background transition-[max-width]',
              narrow ? 'max-w-[390px]' : 'max-w-full'
            )}
          />
        </div>
      )}
    </section>
  )
}

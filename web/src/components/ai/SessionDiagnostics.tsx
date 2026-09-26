// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { getUserConversationDiagnostics } from '@/api/client'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { HighlightedCode } from '@/components/ui/code-block'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { Bug, RefreshCw } from 'lucide-react'

/** Developer-only surface: opt in with ?debug=true, never expose in normal chat. */
export function SessionDiagnostics({ publicId }: { publicId: string }) {
  const [searchParams] = useSearchParams()
  if (searchParams.get('debug') !== 'true') return null
  return <SessionDiagnosticsDialog publicId={publicId} />
}

/** Unmounted outside debug mode, so no diagnostics query can run there. */
function SessionDiagnosticsDialog({ publicId }: { publicId: string }) {
  const [open, setOpen] = useState(false)
  const query = useQuery({
    queryKey: ['conversation-diagnostics', publicId],
    enabled: open,
    retry: false,
    refetchOnWindowFocus: false,
    queryFn: async () => {
      const response = await getUserConversationDiagnostics({
        path: { public_id: publicId },
        throwOnError: true,
      })
      return response.data
    },
  })
  const json =
    query.data && !query.isError ? JSON.stringify(query.data, null, 2) : ''
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-8 gap-1 rounded-full text-xs"
        >
          <Bug className="size-3.5" /> Session JSON
        </Button>
      </DialogTrigger>
      <DialogContent className="flex max-h-[85vh] max-w-4xl flex-col overflow-hidden">
        <DialogTitle>Session diagnostics</DialogTitle>
        <DialogDescription>
          Inspect stored chat events and the native harness session when
          available. Credentials are redacted and export limits are reported in
          the JSON. Messages and project details may still be private; review
          before sharing. This is a snapshot, not a live feed. Refresh to
          inspect pending tools.
        </DialogDescription>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={query.isFetching}
            onClick={() => void query.refetch()}
          >
            <RefreshCw className="mr-2 size-4" /> Refresh
          </Button>
          {json && <CopyButton value={json} label="Copy session JSON" />}
        </div>
        {query.data && !query.isError && (
          <p className="text-xs text-muted-foreground">
            Native session: {query.data.source.native_session_status}.{' '}
            {query.data.source.native_session_note} Stored history:{' '}
            {query.data.returned_message_count} messages.
            {query.data.truncated &&
              ' Export limits were reached; this snapshot is incomplete.'}
          </p>
        )}
        {query.isPending && <Skeleton className="h-64 w-full" />}
        {query.isError && (
          <p role="alert" className="text-sm text-destructive">
            Could not load session diagnostics. Your session may have expired or
            access may have changed. Refresh to retry.
          </p>
        )}
        {json && (
          <pre
            aria-label="Session JSON"
            className="min-h-0 overflow-auto rounded-md border p-3 text-xs"
          >
            <HighlightedCode code={json} language="json" />
          </pre>
        )}
      </DialogContent>
    </Dialog>
  )
}

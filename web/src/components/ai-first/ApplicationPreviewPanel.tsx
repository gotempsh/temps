// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ExternalLink, Loader2, Monitor, RefreshCw } from 'lucide-react'
import { type FormEvent, useCallback, useEffect, useState } from 'react'
import {
  createApplicationPreviewLink,
  createGlobalWorkspacePreviewLink,
  type ApplicationPreviewLinkResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { safePreviewHost } from './application-preview'

async function requestPreviewLink(
  applicationPublicId: string | undefined,
  port: number
): Promise<ApplicationPreviewLinkResponse> {
  const { data } = applicationPublicId
    ? await createApplicationPreviewLink({
        path: { application_public_id: applicationPublicId },
        body: { port, path: '/' },
        throwOnError: true,
      })
    : await createGlobalWorkspacePreviewLink({
        body: { port, path: '/' },
        throwOnError: true,
      })
  return data
}

function previewErrorMessage(error: unknown): string {
  if (error && typeof error === 'object') {
    const payload = error as {
      detail?: unknown
      title?: unknown
      message?: unknown
    }
    for (const candidate of [payload.detail, payload.title, payload.message]) {
      if (typeof candidate === 'string' && candidate.trim()) return candidate
    }
  }
  return 'Temps could not open this sandbox preview.'
}

export function ApplicationPreviewPanel({
  applicationPublicId,
}: {
  applicationPublicId?: string
}) {
  const [portText, setPortText] = useState('3000')
  const [preview, setPreview] = useState<ApplicationPreviewLinkResponse | null>(
    null
  )
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadPreviewPort = useCallback(
    async (port: number) => {
      setLoading(true)
      setError(null)
      try {
        setPreview(await requestPreviewLink(applicationPublicId, port))
      } catch (cause) {
        setPreview(null)
        setError(previewErrorMessage(cause))
      } finally {
        setLoading(false)
      }
    },
    [applicationPublicId]
  )

  useEffect(() => {
    let cancelled = false
    void requestPreviewLink(applicationPublicId, 3000)
      .then((value) => {
        if (!cancelled) setPreview(value)
      })
      .catch((cause: unknown) => {
        if (!cancelled) setError(previewErrorMessage(cause))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [applicationPublicId])

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const port = Number(portText)
    if (!Number.isInteger(port) || port < 1 || port > 65_535) {
      setError('Enter a port between 1 and 65535.')
      return
    }
    void loadPreviewPort(port)
  }

  const host = safePreviewHost(preview?.url ?? null)

  return (
    <div className="flex min-h-full flex-col gap-3">
      <form
        className="flex items-center gap-2 rounded-lg border border-border bg-background p-2"
        onSubmit={submit}
      >
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <Monitor className="size-3.5 shrink-0 text-success" />
          <span className="text-[10px] text-muted-foreground">Port</span>
          <Input
            aria-label="Sandbox preview port"
            className="h-7 w-20 font-mono text-[10px]"
            inputMode="numeric"
            max={65535}
            min={1}
            onChange={(event) => setPortText(event.target.value)}
            type="number"
            value={portText}
          />
        </div>
        <Button
          aria-label="Reload sandbox preview"
          className="size-7"
          disabled={loading}
          size="icon"
          type="submit"
          variant="ghost"
        >
          {loading ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <RefreshCw className="size-3.5" />
          )}
        </Button>
      </form>

      {host && preview ? (
        <section className="overflow-hidden rounded-xl border border-border bg-background shadow-sm">
          <div className="flex items-center gap-2 border-b border-border bg-muted/50 px-2.5 py-2">
            <span className="size-1.5 rounded-full bg-success shadow-[0_0_0_3px_hsl(var(--success)/0.12)]" />
            <span className="min-w-0 flex-1 truncate font-mono text-[9px] text-muted-foreground">
              {host}
            </span>
            <a
              aria-label="Open sandbox preview in a new tab"
              className="rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              href={preview.url}
              rel="noreferrer"
              target="_blank"
            >
              <ExternalLink className="size-3.5" />
            </a>
          </div>
          <iframe
            className="h-[calc(100dvh-11rem)] min-h-[420px] w-full bg-white"
            referrerPolicy="no-referrer"
            sandbox="allow-forms allow-modals allow-popups allow-same-origin allow-scripts"
            src={preview.url}
            title={`Sandbox preview on port ${portText}`}
          />
        </section>
      ) : error ? (
        <section className="rounded-xl border border-amber-500/30 bg-amber-500/5 p-4">
          <p className="text-xs font-medium">Preview unavailable</p>
          <p className="mt-1 text-[10px] leading-5 text-muted-foreground">
            {error}
          </p>
          <p className="mt-2 text-[10px] leading-5 text-muted-foreground">
            Start a development server that listens on 0.0.0.0, then enter its
            port above.
          </p>
        </section>
      ) : (
        <section className="flex min-h-64 items-center justify-center rounded-xl border border-dashed border-border bg-muted/20 text-[10px] text-muted-foreground">
          <Loader2 className="mr-2 size-3.5 animate-spin" /> Connecting to the
          workspace preview…
        </section>
      )}

      <p className="text-[10px] leading-4 text-muted-foreground">
        Preview access is protected by a short-lived browser grant. The grant is
        exchanged by the preview gateway and is never sent to the app.
      </p>
    </div>
  )
}

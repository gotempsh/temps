// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ExternalLink, Loader2, Monitor, RefreshCw } from 'lucide-react'
import { type FormEvent, useCallback, useEffect, useRef, useState } from 'react'
import {
  createApplicationPreviewLink,
  createGlobalWorkspacePreviewLink,
  type ApplicationPreviewLinkResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { showPreviewLoadingPage } from './preview-loading-page'
import {
  previewErrorMessage,
  safePreviewHost,
  previewRenewalPath,
  previewCookieErrorMessage,
} from './application-preview'

async function requestPreviewLink(
  applicationPublicId: string | undefined,
  port: number,
  path = '/'
): Promise<ApplicationPreviewLinkResponse> {
  const { data } = applicationPublicId
    ? await createApplicationPreviewLink({
        path: { application_public_id: applicationPublicId },
        body: { port, path },
        throwOnError: true,
      })
    : await createGlobalWorkspacePreviewLink({
        body: { port, path },
        throwOnError: true,
      })
  return data
}

export function ApplicationPreviewPanel({
  applicationPublicId,
}: {
  applicationPublicId?: string
}) {
  return (
    <PreviewContent
      key={applicationPublicId ?? 'global'}
      applicationPublicId={applicationPublicId}
    />
  )
}

function PreviewContent({
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
  const [cookieBlocked, setCookieBlocked] = useState(false)
  const iframe = useRef<HTMLIFrameElement>(null)
  const generation = useRef(0)
  const lastRenewal = useRef(0)
  const activePort = useRef(3000)
  const activePath = useRef('/')

  const loadPreviewPort = useCallback(
    async (port: number, path = '/') => {
      const request = ++generation.current
      activePort.current = port
      activePath.current = path
      setLoading(true)
      setError(null)
      setCookieBlocked(false)
      try {
        const next = await requestPreviewLink(applicationPublicId, port, path)
        if (request === generation.current) setPreview(next)
      } catch (cause) {
        if (request === generation.current) {
          setPreview(null)
          setError(previewErrorMessage(cause))
        }
      } finally {
        if (request === generation.current) setLoading(false)
      }
    },
    [applicationPublicId]
  )

  const invalidatePendingPreview = useCallback(() => {
    generation.current++
  }, [])

  useEffect(() => {
    const request = ++generation.current
    // The keyed component already starts in its loading state. Only update
    // React state when the external authorization request settles.
    void requestPreviewLink(applicationPublicId, 3000)
      .then((next) => {
        if (request === generation.current) setPreview(next)
      })
      .catch((cause: unknown) => {
        if (request === generation.current) setError(previewErrorMessage(cause))
      })
      .finally(() => {
        if (request === generation.current) setLoading(false)
      })
    return invalidatePendingPreview
  }, [applicationPublicId, invalidatePendingPreview])

  useEffect(() => {
    if (!preview || cookieBlocked) return
    const origin = new URL(preview.url).origin
    const onMessage = (event: MessageEvent) => {
      if (
        event.source !== iframe.current?.contentWindow ||
        event.origin !== origin
      )
        return
      const path = previewRenewalPath(event.data, activePath.current)
      if (path === null) return
      activePath.current = path
      if (Date.now() - lastRenewal.current < 60_000) {
        setError(previewCookieErrorMessage(preview.url))
        setCookieBlocked(true)
        return
      }
      lastRenewal.current = Date.now()
      void loadPreviewPort(activePort.current, path)
    }
    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [preview, cookieBlocked, loadPreviewPort])

  const openPreview = async () => {
    const tab = window.open('about:blank', '_blank')
    if (!tab) {
      setError('Allow pop-ups to open the preview in a new tab.')
      return
    }
    tab.opener = null
    const request = generation.current
    try {
      showPreviewLoadingPage(tab)
      const next = await requestPreviewLink(
        applicationPublicId,
        activePort.current,
        activePath.current
      )
      if (request !== generation.current) {
        tab.close()
        return
      }
      tab.location.replace(next.url)
    } catch (cause) {
      tab.close()
      if (request === generation.current) setError(previewErrorMessage(cause))
    }
  }

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
          <Monitor className="size-3.5 shrink-0 text-muted-foreground" />
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

      {error && preview && !cookieBlocked && (
        <p role="alert" className="text-xs text-destructive">
          {error}
        </p>
      )}
      {cookieBlocked ? (
        <section className="rounded-xl border border-amber-500/30 bg-amber-500/5 p-4">
          <p className="text-xs font-medium">Open this preview in a new tab</p>
          <p
            role="alert"
            className="mt-1 text-xs leading-5 text-muted-foreground"
          >
            {error}
          </p>
          <Button
            className="mt-3"
            onClick={() => void openPreview()}
            size="sm"
            variant="outline"
          >
            <ExternalLink className="mr-2 size-3.5" />
            Open preview in new tab
          </Button>
        </section>
      ) : host && preview ? (
        <section className="overflow-hidden rounded-xl border border-border bg-background shadow-sm">
          <div className="flex items-center gap-2 border-b border-border bg-muted/50 px-2.5 py-2">
            <span className="min-w-0 flex-1 truncate font-mono text-[9px] text-muted-foreground">
              {host}
            </span>
            <button
              aria-label="Open sandbox preview in a new tab"
              className="rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              type="button"
              onClick={() => void openPreview()}
            >
              <ExternalLink className="size-3.5" />
            </button>
          </div>
          <iframe
            ref={iframe}
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
            Check the issue above, then retry. If the app is unreachable, check
            that its server listens on 0.0.0.0 and the selected port.
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

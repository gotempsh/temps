// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getChatSandboxOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { ExternalLink, Monitor, RefreshCw } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import {
  canEmbed,
  parsePreviewTarget,
  previewAddress,
  type PreviewTarget,
} from './chat-preview'
import { CreateSandboxAction } from './ChatSandbox'
import { previewFrameSource } from './preview-session'
import { useChatSandboxWake } from './chat-sandbox-wake'

function PreviewOnboarding({ reason }: { reason: string }) {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <Monitor className="size-7 text-muted-foreground" aria-hidden />
      <p className="text-sm font-medium">No preview yet</p>
      <p className="max-w-xs text-sm text-muted-foreground">
        {reason} Try &ldquo;Create a landing page for a barber shop&rdquo;: the
        page opens here once it is running.
      </p>
    </div>
  )
}

/** The sandbox stopped while idle: it is being started, or could not be. */
export function SandboxWaking({
  error,
  onRetry,
}: {
  error: string | null
  onRetry: () => void
}) {
  if (error !== null) {
    return (
      <div
        role="alert"
        className="flex flex-col items-center gap-2 p-6 text-center"
      >
        <Monitor className="size-7 text-muted-foreground" aria-hidden />
        <p className="text-sm font-medium">The sandbox could not be started</p>
        <p className="max-w-xs text-sm text-muted-foreground">{error}</p>
        <Button type="button" size="sm" variant="outline" onClick={onRetry}>
          Try again
        </Button>
      </div>
    )
  }
  return (
    <div
      className="space-y-3 p-4"
      role="status"
      aria-label="Waking the sandbox"
    >
      <p className="text-sm font-medium">Waking this chat&rsquo;s sandbox</p>
      <p className="text-sm text-muted-foreground">
        It stopped after sitting idle. Its files are intact; the preview opens
        as soon as it is up.
      </p>
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-64 w-full" />
    </div>
  )
}

/**
 * The page the chat's sandbox is serving, shown beside the conversation. The
 * port is the one the assistant last confirmed; the address bar takes any
 * other port or any path on it.
 */
export function ChatPreviewTab({
  conversationPublicId,
}: {
  conversationPublicId: string
}) {
  const status = useQuery(
    getChatSandboxOptions({ path: { public_id: conversationPublicId } })
  )
  const wake = useChatSandboxWake(conversationPublicId)
  const confirmed = status.data?.preview?.port ?? null
  const [target, setTarget] = useState<PreviewTarget | null>(
    confirmed === null ? null : { port: confirmed, path: '/' }
  )
  // What the user is typing; null while the bar just shows where the frame is.
  const [draft, setDraft] = useState<string | null>(null)
  const [reloads, setReloads] = useState(0)
  // The assistant starting a server on another port takes the tab with it.
  const [seenConfirmed, setSeenConfirmed] = useState(confirmed)
  if (confirmed !== seenConfirmed) {
    setSeenConfirmed(confirmed)
    if (confirmed !== null) {
      setTarget({ port: confirmed, path: '/' })
      setDraft(null)
    }
  }

  if (status.isPending) {
    return (
      <div className="space-y-3 p-4" role="status" aria-label="Loading">
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }
  if (status.isError) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load this chat&rsquo;s sandbox. It will try again
        shortly.
      </p>
    )
  }
  const { configured, linked, preview, reason } = status.data
  if (!configured) {
    return (
      <PreviewOnboarding reason={reason ?? 'This chat cannot use a sandbox.'} />
    )
  }
  if (!linked || !preview) {
    return (
      <>
        <PreviewOnboarding reason="This chat has no sandbox yet. When the assistant builds something, it runs in one." />
        {!linked && (
          <CreateSandboxAction conversationPublicId={conversationPublicId} />
        )}
      </>
    )
  }

  if (wake.phase !== 'ready') {
    return <SandboxWaking error={wake.error} onRetry={wake.retry} />
  }

  const url =
    target === null ? null : previewAddress(preview.url_template, target)
  const embeddable = url !== null && canEmbed(url, window.location.protocol)
  const shown = draft ?? url ?? ''
  const parsed =
    draft === null
      ? null
      : parsePreviewTarget(draft, target?.port ?? null, preview.url_template)
  const invalid = draft !== null && draft.trim() !== '' && parsed === null
  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (parsed === null) return
    // Going to the page already shown reloads it, as in a browser.
    if (url !== null && previewAddress(preview.url_template, parsed) === url) {
      setReloads((count) => count + 1)
    }
    setTarget(parsed)
    setDraft(null)
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <form
        onSubmit={submit}
        className="flex shrink-0 items-center gap-1.5 border-b px-3 py-2"
      >
        <label htmlFor="preview-address" className="sr-only">
          Address of the preview
        </label>
        <Input
          id="preview-address"
          value={shown}
          onChange={(event) => setDraft(event.target.value)}
          onFocus={(event) => event.target.select()}
          onBlur={() => setDraft(null)}
          placeholder="Port or path, e.g. 3000 or /pricing"
          spellCheck={false}
          autoComplete="off"
          autoCapitalize="off"
          className="h-8 min-w-0 flex-1 font-mono text-xs"
          aria-invalid={invalid}
          aria-describedby={invalid ? 'preview-address-hint' : undefined}
        />
        {url && (
          <>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="size-8 shrink-0"
              onClick={() => setReloads((count) => count + 1)}
              aria-label="Reload the preview"
            >
              <RefreshCw className="size-4" />
            </Button>
            <Button
              asChild
              size="icon"
              variant="ghost"
              className="size-8 shrink-0"
              aria-label="Open the preview in a new tab"
            >
              <a href={url} target="_blank" rel="noopener noreferrer">
                <ExternalLink className="size-4" />
              </a>
            </Button>
          </>
        )}
      </form>
      {invalid && (
        <p
          id="preview-address-hint"
          role="alert"
          className="shrink-0 border-b px-3 py-1.5 text-xs text-destructive"
        >
          Enter a path such as /pricing, a port such as 5173, or both
          (:5173/about). Only this sandbox&rsquo;s own pages can be shown.
        </p>
      )}
      {url === null ? (
        <p className="p-4 text-sm text-muted-foreground">
          Enter a port above (for example 3000), or ask the assistant to run it
          and show it. It appears here as soon as the assistant confirms it
          answers.
        </p>
      ) : embeddable && linked ? (
        <PreviewFrame
          sandboxId={linked.sandbox_id}
          url={url}
          reloads={reloads}
        />
      ) : (
        <p className="p-4 text-sm text-muted-foreground">
          This console is served over HTTPS and the preview is not, so the
          browser will not show it here. Open it in a new tab instead.
        </p>
      )}
    </div>
  )
}

/**
 * The frame showing `url`. It loads through a fresh one-time preview
 * session (see `previewFrameSource`) on every load, so a sandbox whose
 * preview asks for a Temps login shows the page, not a sign-in redirect.
 */
function PreviewFrame({
  sandboxId,
  url,
  reloads,
}: {
  sandboxId: string
  url: string
  reloads: number
}) {
  const source = useQuery({
    queryKey: ['sandbox-preview-frame', sandboxId, url, reloads],
    queryFn: () => previewFrameSource(sandboxId, url),
    // A session is single use: never reuse one, and never swap the frame's
    // page in the background.
    gcTime: 0,
    staleTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })
  if (source.isPending) {
    return (
      <div
        className="space-y-3 p-4"
        role="status"
        aria-label="Opening the preview"
      >
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }
  if (source.isError) {
    return (
      <p role="alert" className="p-4 text-sm text-muted-foreground">
        {source.error.message} Open it in a new tab instead.
      </p>
    )
  }
  return (
    <iframe
      key={`${url}:${reloads}`}
      src={source.data}
      title="Preview of the app running in this chat's sandbox"
      className="min-h-0 w-full flex-1 bg-white"
      sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-modals"
    />
  )
}

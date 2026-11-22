// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ArrowUp, Bell, Pause, Play, X } from 'lucide-react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import { Button } from '@/components/ui/button'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { Textarea } from '@/components/ui/textarea'
import { Skeleton } from '@/components/ui/skeleton'
import {
  listChatPluginWatches,
  changeChatPluginWatch,
} from '@/api/client/sdk.gen'
import type { WatchChange } from '@/api/client/types.gen'
import {
  refreshWatchTranscript,
  requestChatContinuation,
} from './chat-blueprint'

export function ChatPluginWatches({
  chat,
  readOnly = false,
}: {
  chat: string
  readOnly?: boolean
}) {
  const [open, setOpen] = useState(false)
  const [request, setRequest] = useState('')
  const [removeTarget, setRemoveTarget] = useState<{
    id: string
    label: string
  } | null>(null)
  const submitRequest = useMutation({
    mutationFn: async (text: string) => {
      if (
        !(await requestChatContinuation(
          chat,
          `Set up a watch in this chat. ${text}`
        ))
      ) {
        throw new Error(
          'The conversation could not accept the request. Try again when the chat is ready.'
        )
      }
    },
    retry: false,
    onSuccess: () => {
      setRequest('')
      setOpen(false)
    },
  })
  const panelRef = useRef<HTMLDivElement>(null)
  const watches = useQuery({
    queryKey: ['plugin-watches', chat],
    queryFn: async () =>
      (await listChatPluginWatches({ path: { chat }, throwOnError: true }))
        .data,
    refetchInterval: open ? 5000 : 30000,
  })
  const mutation = useMutation({
    mutationFn: async (body: WatchChange) =>
      changeChatPluginWatch({ path: { chat }, body, throwOnError: true }),
    onSuccess: async () => {
      await watches.refetch()
      toast.success('Chat watches updated')
    },
    onError: () =>
      toast.error(
        'Could not update this watch. Check your session and access to the resource.'
      ),
  })
  const panel = watches.data
  const previousDeliveries = useRef<{
    chat: string
    counts: Map<string, number>
  } | null>(null)
  const refreshFailure = useRef(false)
  useEffect(() => {
    if (!panel) return
    let cancelled = false
    const baseline = previousDeliveries.current
    const counts = new Map(panel.watches.map((w) => [w.id, w.delivered]))
    if (baseline?.chat !== chat) {
      previousDeliveries.current = { chat, counts }
      refreshFailure.current = false
      return
    }
    const changed = panel.watches.filter((watch) => {
      const before = baseline.counts.get(watch.id) ?? 0
      return watch.mode === 'notify' && watch.delivered > before
    })
    if (!changed.length) {
      previousDeliveries.current = { chat, counts }
      return
    }
    async function refresh() {
      const applied = await refreshWatchTranscript(chat).catch(() => false)
      if (cancelled) return
      if (!applied) {
        if (!refreshFailure.current)
          toast.error(
            'Could not refresh watch notifications. Retrying automatically.'
          )
        refreshFailure.current = true
        return
      }
      previousDeliveries.current = { chat, counts }
      refreshFailure.current = false
      for (const watch of changed)
        toast.info(`${watch.label || watch.subject} · ${watch.source}`)
    }
    void refresh()
    return () => {
      cancelled = true
    }
  }, [chat, panel, watches.dataUpdatedAt])
  const active =
    panel?.watches.filter((w) => !w.paused && !w.blocked).length ?? 0
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="gap-1.5"
          aria-label={`Plugin watches, ${active} active`}
          title="Watches"
        >
          <Bell className="size-4" />
          <span className="max-sm:sr-only">Watches</span>
          {active > 0 && (
            <span className="rounded bg-muted px-1.5 text-xs tabular-nums">
              {active}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        collisionPadding={12}
        aria-label="Chat watches"
        ref={panelRef}
        tabIndex={-1}
        onOpenAutoFocus={(event) => {
          if (
            panel?.watches.length ||
            window.matchMedia('(pointer: coarse)').matches
          ) {
            event.preventDefault()
            panelRef.current?.focus()
          }
        }}
        className="w-[min(24rem,calc(100vw-1.5rem))] max-h-[var(--radix-popover-content-available-height)] overflow-y-auto p-0"
      >
        <header className="px-4 pt-4 pb-3">
          <h2 className="text-sm font-semibold">Watches</h2>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
            Your agent can follow changes and respond in this chat.
          </p>
        </header>
        {watches.isPending && <Skeleton className="mx-4 mb-4 h-14" />}
        {watches.isError && (
          <div
            role="alert"
            className="flex items-center justify-between gap-2 px-4 pb-3 text-sm"
          >
            Couldn’t load watches.{' '}
            <Button
              size="sm"
              variant="outline"
              onClick={() => void watches.refetch()}
            >
              Retry
            </Button>
          </div>
        )}
        {panel && (
          <>
            {panel.watches.length > 0 && (
              <ul className="divide-y border-t">
                {panel.watches.map((w) => {
                  const label = w.label || w.subject
                  const suspended = w.paused || w.blocked
                  const source = panel.sources.find(
                    (s) => s.plugin === w.plugin && s.id === w.source
                  )
                  const status = w.blocked
                    ? 'Access blocked'
                    : w.paused
                      ? 'Paused'
                      : w.mode === 'wake'
                        ? 'Watching'
                        : 'Notify only'
                  return (
                    <li
                      key={w.id}
                      className="flex items-center gap-1 px-3 py-2.5"
                    >
                      <div className="min-w-0 flex-1 px-1">
                        <p className="break-words text-sm font-medium">
                          {label}
                        </p>
                        <p className="mt-0.5 text-xs text-muted-foreground">
                          {status} · {source?.label ?? w.source} · {w.delivered}{' '}
                          delivered
                        </p>
                      </div>
                      {!readOnly && (
                        <>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8 shrink-0"
                            disabled={mutation.isPending}
                            title={suspended ? 'Resume watch' : 'Pause watch'}
                            aria-label={`${suspended ? 'Resume' : 'Pause'} ${label}`}
                            onClick={() =>
                              mutation.mutate({
                                action: suspended ? 'resume' : 'pause',
                                id: w.id,
                              })
                            }
                          >
                            {suspended ? (
                              <Play className="size-4" />
                            ) : (
                              <Pause className="size-4" />
                            )}
                          </Button>
                          <Button
                            variant="ghost"
                            size="icon"
                            className="size-8 shrink-0"
                            disabled={mutation.isPending}
                            title="Remove watch"
                            aria-label={`Remove ${label}`}
                            onClick={() => setRemoveTarget({ id: w.id, label })}
                          >
                            <X className="size-4" />
                          </Button>
                        </>
                      )}
                    </li>
                  )
                })}
              </ul>
            )}
            {panel.sources.length === 0 ? (
              <p className="px-4 pb-4 text-sm text-muted-foreground">
                No plugin has watch sources enabled.{' '}
                <Link
                  to="/settings/plugins?tab=running"
                  className="text-foreground underline"
                >
                  Manage plugins
                </Link>
              </p>
            ) : (
              !readOnly && (
                <form
                  className="px-4 pb-4 pt-1"
                  onSubmit={(e) => {
                    e.preventDefault()
                    if (!request.trim()) return
                    submitRequest.mutate(request.trim())
                  }}
                >
                  <div className="rounded-xl border bg-muted/30 p-2 focus-within:border-ring">
                    <Textarea
                      aria-label="What should the agent watch?"
                      placeholder="What should I watch, and what should I do when it changes?"
                      value={request}
                      disabled={submitRequest.isPending}
                      onChange={(e) => setRequest(e.target.value)}
                      rows={2}
                      onKeyDown={(event) => {
                        if (
                          event.key === 'Enter' &&
                          !event.shiftKey &&
                          !event.nativeEvent.isComposing
                        ) {
                          event.preventDefault()
                          event.currentTarget.form?.requestSubmit()
                        }
                      }}
                      className="min-h-14 max-h-36 resize-none border-0 bg-transparent px-1 py-1 text-sm shadow-none focus-visible:outline-none"
                    />
                    <div className="flex justify-end pt-1">
                      <Button
                        size="sm"
                        type="submit"
                        disabled={!request.trim() || submitRequest.isPending}
                        className="h-7 gap-1.5 rounded-lg px-2.5"
                      >
                        Ask agent <ArrowUp className="size-3.5" />
                      </Button>
                    </div>
                  </div>
                  {submitRequest.error && (
                    <p role="alert" className="text-sm text-destructive">
                      {submitRequest.error.message}
                    </p>
                  )}
                </form>
              )
            )}
          </>
        )}
      </PopoverContent>
      <ConfirmDialog
        open={removeTarget !== null}
        title="Remove watch?"
        description={`Removing ${removeTarget?.label ?? ''} stops future deliveries to this chat.`}
        confirmLabel="Remove watch"
        onOpenChange={(open) => {
          if (!open) setRemoveTarget(null)
        }}
        onConfirm={() => {
          if (removeTarget)
            mutation.mutate({ action: 'remove', id: removeTarget.id })
        }}
      />
    </Popover>
  )
}

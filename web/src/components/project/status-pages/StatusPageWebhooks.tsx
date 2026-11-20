// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createStatusPageWebhookMutation,
  deleteStatusPageWebhookMutation,
  listStatusPageWebhooksOptions,
  updateStatusPageWebhookMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  StatusPageResponse,
  StatusPageWebhookResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Rss, Trash2, Webhook } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import {
  deliveryState,
  MAX_WEBHOOK_URL_CHARS,
  webhookSchema,
  type WebhookValues,
} from './notifications'
import { operationQueries } from './status-page-form'

const TONE_VARIANTS = {
  success: 'success-subtle',
  warning: 'warning-subtle',
  neutral: 'neutral-subtle',
} as const

/** The page's Atom feed address, or why it has none. */
export function StatusPageFeedCard({ page }: { page: StatusPageResponse }) {
  return (
    <Card>
      <CardContent className="space-y-2 p-4">
        <h3 className="flex items-center gap-2 font-semibold">
          <Rss className="size-4" />
          Atom feed
        </h3>
        {page.public_feed_url ? (
          <div className="flex items-center gap-2">
            <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1 text-xs [overflow-wrap:anywhere]">
              {page.public_feed_url}
            </code>
            <CopyButton value={page.public_feed_url} label="Copy feed URL" />
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            {page.has_password
              ? 'A password-protected page has no feed: a feed reader cannot sign in.'
              : 'The feed gets an address once this install has a public URL (see Overview).'}
          </p>
        )}
      </CardContent>
    </Card>
  )
}

/** Signed webhooks of a status page (ADR 108 D2). */
export function StatusPageWebhooks({ page }: { page: StatusPageResponse }) {
  const queryClient = useQueryClient()
  const path = { status_page_id: page.id }
  const [secret, setSecret] = useState<string | null>(null)
  const [target, setTarget] = useState<StatusPageWebhookResponse | null>(null)
  const list = useQuery(
    listStatusPageWebhooksOptions({ path, query: { page: 1, page_size: 100 } })
  )
  const form = useForm<WebhookValues>({
    resolver: zodResolver(webhookSchema),
    defaultValues: { url: '' },
  })
  const invalidate = () =>
    queryClient.invalidateQueries(operationQueries('listStatusPageWebhooks'))

  const create = useMutation({
    ...createStatusPageWebhookMutation(),
    meta: { errorTitle: 'Failed to add webhook' },
    onSuccess: async (created) => {
      await invalidate()
      form.reset({ url: '' })
      setSecret(created.secret)
      toast.success('Webhook added. Copy its signing secret now.')
    },
  })
  const update = useMutation({
    ...updateStatusPageWebhookMutation(),
    meta: { errorTitle: 'Failed to change webhook' },
    onSuccess: async (saved) => {
      await invalidate()
      toast.success(saved.enabled ? 'Webhook resumed.' : 'Webhook paused.')
    },
  })
  const remove = useMutation({
    ...deleteStatusPageWebhookMutation(),
    meta: { errorTitle: 'Failed to delete webhook' },
    onSuccess: async () => {
      await invalidate()
      toast.success('Webhook deleted.')
      setTarget(null)
    },
    onError: () => setTarget(null),
  })

  return (
    <Card>
      <CardContent className="space-y-4 p-4">
        <div className="space-y-1">
          <h3 className="flex items-center gap-2 font-semibold">
            <Webhook className="size-4" />
            Webhooks
          </h3>
          <p className="text-sm text-muted-foreground">
            Post every public update to your own endpoint, e.g. a chat channel
            relay. Each request is signed with X-Temps-Signature.
          </p>
        </div>

        {secret && (
          <Alert variant="warning">
            <AlertTitle>Signing secret: shown once</AlertTitle>
            <AlertDescription className="space-y-2">
              <div className="flex items-center gap-2">
                <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1 text-xs [overflow-wrap:anywhere]">
                  {secret}
                </code>
                <CopyButton value={secret} label="Copy signing secret" />
              </div>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setSecret(null)}
              >
                I have stored it
              </Button>
            </AlertDescription>
          </Alert>
        )}

        <Form {...form}>
          <form
            className="flex flex-col gap-3 sm:flex-row sm:items-start"
            onSubmit={form.handleSubmit((values) =>
              create.mutate({ path, body: { url: values.url.trim() } })
            )}
          >
            <FormField
              control={form.control}
              name="url"
              render={({ field }) => (
                <FormItem className="w-full sm:max-w-md">
                  <FormLabel className="sr-only">Webhook URL</FormLabel>
                  <FormControl>
                    <Input
                      placeholder="https://hooks.example.com/status"
                      maxLength={MAX_WEBHOOK_URL_CHARS}
                      autoComplete="off"
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button type="submit" variant="outline" disabled={create.isPending}>
              {create.isPending ? 'Adding...' : 'Add webhook'}
            </Button>
          </form>
        </Form>

        {list.isPending && <Skeleton className="h-20 w-full" />}
        {list.isError && (
          <ErrorAlert
            title="Could not load webhooks"
            description={
              list.error instanceof Error
                ? list.error.message
                : String(list.error)
            }
            retry={() => list.refetch()}
          />
        )}
        {list.data && list.data.items.length > 0 && (
          <ul className="divide-y rounded-md border">
            {list.data.items.map((webhook) => {
              const state = deliveryState(webhook)
              return (
                <li
                  key={webhook.id}
                  className="flex flex-col gap-2 p-3 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="min-w-0 space-y-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm [overflow-wrap:anywhere]">
                        {webhook.url}
                      </span>
                      <Badge variant={TONE_VARIANTS[state.tone]}>
                        {state.label}
                      </Badge>
                    </div>
                    {state.detail && (
                      <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                        {state.detail}
                      </p>
                    )}
                  </div>
                  <div className="flex shrink-0 items-center gap-2">
                    <Switch
                      checked={webhook.enabled}
                      disabled={update.isPending}
                      aria-label={`Deliver events to ${webhook.url}`}
                      onCheckedChange={(enabled) =>
                        update.mutate({
                          path: { ...path, webhook_id: webhook.id },
                          body: { enabled },
                        })
                      }
                    />
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Delete ${webhook.url}`}
                      onClick={() => setTarget(webhook)}
                    >
                      <Trash2 className="size-4" />
                    </Button>
                  </div>
                </li>
              )
            })}
          </ul>
        )}
      </CardContent>
      <ConfirmDeleteDialog
        open={target !== null}
        onOpenChange={(open) => {
          if (!open) setTarget(null)
        }}
        resourceType="webhook"
        resourceName={target?.url ?? ''}
        consequences="Events still waiting for it are dropped."
        isPending={remove.isPending}
        onConfirm={() => {
          if (target) {
            remove.mutate({ path: { ...path, webhook_id: target.id } })
          }
        }}
      />
    </Card>
  )
}

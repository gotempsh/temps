// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deleteStatusPageSubscriberMutation,
  getStatusPageSubscriptionsOptions,
  listStatusPageSubscribersOptions,
  updateStatusPageSubscriptionsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { exportStatusPageSubscribers } from '@/api/client'
import type {
  StatusPageResponse,
  SubscriberResponse,
  SubscriptionSettingsResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { saveBlobInBrowser } from '@/pages/service-object-download'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { Download, Mail, Trash2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { operationQueries } from './status-page-form'
import {
  pageAfterRemoval,
  senderDomainProblem,
  senderAfterRefetch,
  settingsRequest,
  subscriptionBlockers,
  subscriptionSettingsSchema,
  SUBSCRIBERS_PAGE_SIZE,
  switchRequest,
  type SubscriptionSettingsValues,
} from './subscriptions'

/** Email subscriptions of a status page (ADR 108 D2). */
export function StatusPageSubscribers({ page }: { page: StatusPageResponse }) {
  const path = { status_page_id: page.id }
  const settings = useQuery(getStatusPageSubscriptionsOptions({ path }))

  if (settings.isPending) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-40 w-full" />
        <Skeleton className="h-48 w-full" />
      </div>
    )
  }
  if (settings.isError) {
    return (
      <ErrorAlert
        title="Could not load subscription settings"
        description={
          settings.error instanceof Error
            ? settings.error.message
            : String(settings.error)
        }
        retry={() => settings.refetch()}
      />
    )
  }
  return (
    <div className="space-y-6">
      <SubscriptionSettingsCard page={page} settings={settings.data} />
      <SubscriberList page={page} settings={settings.data} />
    </div>
  )
}

function SubscriptionSettingsCard({
  page,
  settings,
}: {
  page: StatusPageResponse
  settings: SubscriptionSettingsResponse
}) {
  const queryClient = useQueryClient()
  const blockers = subscriptionBlockers(settings)
  const form = useForm<SubscriptionSettingsValues>({
    resolver: zodResolver(subscriptionSettingsSchema),
    defaultValues: { sender: settings.sender ?? '' },
  })
  useEffect(() => {
    const sender = senderAfterRefetch(settings.sender, form.formState.isDirty)
    if (sender !== null) form.reset({ sender })
  }, [settings.sender, form])

  const save = useMutation({
    ...updateStatusPageSubscriptionsMutation(),
    meta: { errorTitle: 'Failed to save subscription settings' },
    onSuccess: async (saved, variables) => {
      // Switching off leaves an unsaved sender edit in the field.
      if (variables.body.sender !== undefined) {
        form.reset({ sender: saved.sender ?? '' })
      }
      await queryClient.invalidateQueries(
        operationQueries('getStatusPageSubscriptions')
      )
      await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
      toast.success(
        saved.enabled
          ? 'Subscriptions are on. The public page shows the subscribe form.'
          : 'Subscription settings saved.'
      )
    },
  })

  const submit = (enabled: boolean) =>
    form.handleSubmit((values) => {
      const problem = senderDomainProblem(
        values.sender,
        settings.verified_domains
      )
      if (problem) {
        form.setError('sender', { message: problem })
        return
      }
      save.mutate({
        path: { status_page_id: page.id },
        body: settingsRequest(values, enabled),
      })
    })()

  const toggle = (next: boolean) => {
    if (!next) {
      save.mutate({
        path: { status_page_id: page.id },
        body: switchRequest(false, form.getValues()),
      })
      return
    }
    submit(true)
  }

  const canEnable = blockers.length === 0 && Boolean(settings.sender)

  return (
    <Card>
      <CardContent className="space-y-4 p-4">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <h3 className="flex items-center gap-2 font-semibold">
            <Mail className="size-4" />
            Email updates for visitors
          </h3>
          <div className="flex items-center gap-2">
            <Switch
              id="subscriptions-enabled"
              checked={settings.enabled}
              disabled={save.isPending || (!settings.enabled && !canEnable)}
              onCheckedChange={toggle}
              aria-label="Let visitors subscribe by email"
            />
            <label htmlFor="subscriptions-enabled" className="text-sm">
              {settings.enabled ? 'On' : 'Off'}
            </label>
          </div>
        </div>

        {blockers.map((blocker) => (
          <Alert key={blocker.reason} variant="warning">
            <AlertTitle>{blocker.reason}</AlertTitle>
            {blocker.setupPath && (
              <AlertDescription>
                <Link className="underline" to={blocker.setupPath}>
                  Set it up
                </Link>{' '}
                and come back to switch subscriptions on.
              </AlertDescription>
            )}
          </Alert>
        ))}

        <Form {...form}>
          <form
            className="flex flex-col gap-3 sm:flex-row sm:items-end"
            onSubmit={(event) => {
              event.preventDefault()
              submit(settings.enabled)
            }}
          >
            <FormField
              control={form.control}
              name="sender"
              render={({ field }) => (
                <FormItem className="w-full sm:max-w-sm">
                  <FormLabel>Sender address</FormLabel>
                  <FormControl>
                    <Input
                      placeholder={
                        settings.verified_domains[0]
                          ? `status@${settings.verified_domains[0]}`
                          : 'status@example.com'
                      }
                      autoComplete="off"
                      {...field}
                    />
                  </FormControl>
                  {settings.verified_domains.length > 0 && (
                    <FormDescription>
                      At a verified domain:{' '}
                      {settings.verified_domains.join(', ')}
                    </FormDescription>
                  )}
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button
              type="submit"
              variant="outline"
              disabled={save.isPending || !form.formState.isDirty}
            >
              Save sender
            </Button>
          </form>
        </Form>
      </CardContent>
    </Card>
  )
}

function SubscriberList({
  page,
  settings,
}: {
  page: StatusPageResponse
  settings: SubscriptionSettingsResponse
}) {
  const queryClient = useQueryClient()
  const [pageNumber, setPageNumber] = useState(1)
  const [target, setTarget] = useState<SubscriberResponse | null>(null)
  const list = useQuery(
    listStatusPageSubscribersOptions({
      path: { status_page_id: page.id },
      query: { page: pageNumber, page_size: SUBSCRIBERS_PAGE_SIZE },
    })
  )
  const remove = useMutation({
    ...deleteStatusPageSubscriberMutation(),
    meta: { errorTitle: 'Failed to delete subscriber' },
    onSuccess: async () => {
      setPageNumber((current) =>
        pageAfterRemoval(current, list.data?.total ?? 0, SUBSCRIBERS_PAGE_SIZE)
      )
      await queryClient.invalidateQueries(
        operationQueries('listStatusPageSubscribers')
      )
      await queryClient.invalidateQueries(
        operationQueries('getStatusPageSubscriptions')
      )
      toast.success('Subscriber deleted. They get no more mail.')
      setTarget(null)
    },
    onError: () => setTarget(null),
  })

  const exportCsv = useMutation({
    mutationFn: async () => {
      const { data } = await exportStatusPageSubscribers({
        path: { status_page_id: page.id },
        parseAs: 'blob',
        throwOnError: true,
      })
      saveBlobInBrowser(
        data as unknown as Blob,
        `status-page-${page.id}-subscribers.csv`
      )
    },
    meta: { errorTitle: 'Failed to export subscribers' },
  })

  const total = list.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / SUBSCRIBERS_PAGE_SIZE))

  return (
    <Card>
      <CardContent className="space-y-4 p-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <h3 className="font-semibold">
            Subscribers{' '}
            <span className="text-sm font-normal text-muted-foreground">
              {settings.confirmed_count} confirmed, {settings.pending_count}{' '}
              waiting for confirmation
            </span>
          </h3>
          <Button
            variant="outline"
            size="sm"
            onClick={() => exportCsv.mutate()}
            disabled={exportCsv.isPending || total === 0}
          >
            <Download className="mr-2 size-4" />
            Export CSV
          </Button>
        </div>
        {list.isPending && <Skeleton className="h-32 w-full" />}
        {list.isError && (
          <ErrorAlert
            title="Could not load subscribers"
            description={
              list.error instanceof Error
                ? list.error.message
                : String(list.error)
            }
            retry={() => list.refetch()}
          />
        )}
        {list.data && list.data.items.length === 0 && (
          <p className="text-sm text-muted-foreground">
            {settings.enabled
              ? 'Nobody has subscribed yet.'
              : 'Switch subscriptions on to show the subscribe form on the public page.'}
          </p>
        )}
        {list.data && list.data.items.length > 0 && (
          <div className="@container rounded-md border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Email</TableHead>
                  <TableHead>State</TableHead>
                  <TableHead className="hidden @xl:table-cell">
                    Subscribed
                  </TableHead>
                  <TableHead className="w-12">
                    <span className="sr-only">Actions</span>
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {list.data.items.map((subscriber) => (
                  <TableRow key={subscriber.id}>
                    <TableCell className="[overflow-wrap:anywhere]">
                      {subscriber.email}
                      <div className="text-xs text-muted-foreground @xl:hidden">
                        {formatDistanceToNow(new Date(subscriber.created_at), {
                          addSuffix: true,
                        })}
                      </div>
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant={
                          subscriber.state === 'confirmed'
                            ? 'success-subtle'
                            : 'neutral-subtle'
                        }
                      >
                        {subscriber.state === 'confirmed'
                          ? 'Confirmed'
                          : 'Pending'}
                      </Badge>
                    </TableCell>
                    <TableCell className="hidden text-sm text-muted-foreground @xl:table-cell">
                      {formatDistanceToNow(new Date(subscriber.created_at), {
                        addSuffix: true,
                      })}
                    </TableCell>
                    <TableCell>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={`Delete ${subscriber.email}`}
                        onClick={() => setTarget(subscriber)}
                      >
                        <Trash2 className="size-4" />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
        {total > SUBSCRIBERS_PAGE_SIZE && (
          <ResponsivePagination
            page={pageNumber}
            pageSize={SUBSCRIBERS_PAGE_SIZE}
            total={total}
            totalPages={Math.max(totalPages, pageNumber)}
            onPageChange={setPageNumber}
            ariaLabel="Subscriber pagination"
          />
        )}
      </CardContent>
      <ConfirmDeleteDialog
        open={target !== null}
        onOpenChange={(open) => {
          if (!open) setTarget(null)
        }}
        resourceType="subscriber"
        resourceName={target?.email ?? ''}
        consequences="The address is removed from this page and gets no more mail."
        isPending={remove.isPending}
        onConfirm={() => {
          if (target) {
            remove.mutate({
              path: { status_page_id: page.id, subscriber_id: target.id },
            })
          }
        }}
      />
    </Card>
  )
}

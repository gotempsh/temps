// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { z } from 'zod'
import { Send, Trash2 } from 'lucide-react'
import {
  createLogIngestKeyMutation,
  listLogIngestKeysOptions,
  listLogIngestKeysQueryKey,
  revokeLogIngestKeyMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  CreatedLogIngestKeyResponse,
  LogIngestKeyResponse,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { CopyButton } from '@/components/ui/copy-button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  curlSnippet,
  fluentBitSnippet,
  LOG_INGEST_PATH,
  logIngestUrl,
  vectorSnippet,
} from '@/lib/log-ingest'
import { PublicCallbackUrl } from '@/components/settings/PublicCallbackUrl'
import { usePublicIngress } from '@/hooks/usePublicIngress'

const createSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Name is required')
    .max(100, 'Name must be 100 characters or fewer'),
  default_service: z
    .string()
    .trim()
    .max(63, 'Service must be 63 characters or fewer'),
})
type CreateForm = z.infer<typeof createSchema>

/**
 * "Send logs": per-service keys for the NDJSON ingest endpoint, so a host,
 * database or app Temps did not deploy can ship logs through Vector, Fluent
 * Bit or plain HTTP.
 */
export function LogIngestKeys({
  projectId,
}: {
  projectId: number | undefined
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Button
        size="sm"
        variant="outline"
        className="h-9 gap-2"
        onClick={() => setOpen(true)}
      >
        <Send className="size-4" />
        <span className="max-sm:sr-only">Send logs</span>
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Send logs from outside Temps</DialogTitle>
          </DialogHeader>
          {projectId ? (
            <LogIngestKeysBody projectId={projectId} enabled={open} />
          ) : (
            <p className="text-sm text-muted-foreground">
              Keys belong to a service. Filter by a service (type{' '}
              <code>project:</code> in the search) to create one, then point
              Vector, Fluent Bit or any HTTP client at it.
            </p>
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}

function LogIngestKeysBody({
  projectId,
  enabled,
}: {
  projectId: number
  enabled: boolean
}) {
  const queryClient = useQueryClient()
  const path = { project_id: projectId }
  const [created, setCreated] = useState<CreatedLogIngestKeyResponse | null>(
    null
  )
  const [revokeTarget, setRevokeTarget] = useState<LogIngestKeyResponse | null>(
    null
  )
  const keys = useQuery({
    ...listLogIngestKeysOptions({ path }),
    enabled,
  })
  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: listLogIngestKeysQueryKey({ path }),
    })
  const form = useForm<CreateForm>({
    resolver: zodResolver(createSchema),
    defaultValues: { name: '', default_service: '' },
  })
  const create = useMutation({
    ...createLogIngestKeyMutation(),
    meta: { errorTitle: 'Failed to create the ingest key' },
    onSuccess: (key) => {
      toast.success(`Created "${key.name}"`)
      setCreated(key)
      form.reset()
      void invalidate()
    },
  })
  const revoke = useMutation({
    ...revokeLogIngestKeyMutation(),
    meta: { errorTitle: 'Failed to revoke the ingest key' },
    onSuccess: () => {
      toast.success('Ingest key revoked')
      setRevokeTarget(null)
      void invalidate()
    },
  })
  const submit = form.handleSubmit((values) =>
    create.mutate({
      path,
      body: {
        name: values.name,
        default_service: values.default_service || null,
      },
    })
  )
  const ingress = usePublicIngress()
  const url = ingress.baseUrl === null ? null : logIngestUrl(ingress.baseUrl)
  const active = (keys.data?.data ?? []).filter((key) => key.is_active)

  return (
    <div className="space-y-5">
      {created ? (
        <div className="space-y-3 rounded-md border p-3">
          <div className="space-y-1">
            <p className="text-sm font-medium">
              Copy this key now. It is not shown again.
            </p>
            <div className="flex items-center gap-2">
              <code className="min-w-0 flex-1 rounded bg-muted px-2 py-1 text-xs [overflow-wrap:anywhere]">
                {created.key}
              </code>
              <CopyButton value={created.key} label="Copy the ingest key" />
            </div>
          </div>
          {url === null ? (
            <PublicCallbackUrl
              path={LOG_INGEST_PATH}
              prefix="POST"
              label="Copy the ingest URL"
            />
          ) : (
            <Snippets url={url} secret={created.key} />
          )}
          <Button size="sm" variant="outline" onClick={() => setCreated(null)}>
            Done
          </Button>
        </div>
      ) : (
        <form onSubmit={submit} className="space-y-3">
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label htmlFor="log-ingest-name">Key name</Label>
              <Input
                id="log-ingest-name"
                placeholder="e.g. web-01 Vector"
                {...form.register('name')}
              />
              {form.formState.errors.name && (
                <p className="text-xs text-destructive">
                  {form.formState.errors.name.message}
                </p>
              )}
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="log-ingest-service">
                Service label (optional)
              </Label>
              <Input
                id="log-ingest-service"
                placeholder="Used when a line names none"
                {...form.register('default_service')}
              />
              {form.formState.errors.default_service && (
                <p className="text-xs text-destructive">
                  {form.formState.errors.default_service.message}
                </p>
              )}
            </div>
          </div>
          <Button type="submit" size="sm" disabled={create.isPending}>
            Create key
          </Button>
        </form>
      )}

      <div className="space-y-2">
        <p className="text-sm font-medium">Active keys</p>
        {keys.isPending ? (
          <div className="space-y-1">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
          </div>
        ) : keys.error ? (
          <div className="flex items-center gap-2 text-sm">
            <span>Keys could not be loaded.</span>
            <Button
              size="sm"
              variant="outline"
              onClick={() => void keys.refetch()}
            >
              Retry
            </Button>
          </div>
        ) : active.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No keys yet. Create one to start sending logs.
          </p>
        ) : (
          <ul className="divide-y rounded-md border" aria-label="Active keys">
            {active.map((key) => (
              <li key={key.id} className="flex items-center gap-2 px-3 py-2">
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">{key.name}</p>
                  <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
                    <code>{key.key_prefix}…</code> · service{' '}
                    {key.default_service} ·{' '}
                    {key.last_used_at
                      ? `${key.line_count.toLocaleString()} lines, last ${new Date(key.last_used_at).toLocaleString()}`
                      : 'never used'}
                  </p>
                </div>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-8"
                  aria-label={`Revoke ${key.name}`}
                  title="Revoke"
                  onClick={() => setRevokeTarget(key)}
                >
                  <Trash2 className="size-4" />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </div>

      <ConfirmDeleteDialog
        open={revokeTarget !== null}
        onOpenChange={(next) => !next && setRevokeTarget(null)}
        resourceType="ingest key"
        resourceName={revokeTarget?.name ?? ''}
        consequences="Collectors using it are refused from the next batch. Lines already sent are kept."
        isPending={revoke.isPending}
        onConfirm={() =>
          revokeTarget &&
          revoke.mutate({ path: { ...path, key_id: revokeTarget.id } })
        }
      />
    </div>
  )
}

function Snippets({ url, secret }: { url: string; secret: string }) {
  const snippets = [
    { id: 'curl', label: 'curl', text: curlSnippet(url, secret) },
    { id: 'vector', label: 'Vector', text: vectorSnippet(url, secret) },
    {
      id: 'fluent-bit',
      label: 'Fluent Bit',
      text: fluentBitSnippet(url, secret),
    },
  ]
  return (
    <Tabs defaultValue="curl">
      <TabsList>
        {snippets.map((s) => (
          <TabsTrigger key={s.id} value={s.id}>
            {s.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {snippets.map((s) => (
        <TabsContent key={s.id} value={s.id} className="relative">
          <pre className="whitespace-pre-wrap rounded bg-muted p-3 pr-10 text-xs [overflow-wrap:anywhere]">
            {s.text}
          </pre>
          <CopyButton
            value={s.text}
            label={`Copy the ${s.label} example`}
            className="absolute right-2 top-2"
          />
        </TabsContent>
      ))}
    </Tabs>
  )
}

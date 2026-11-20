// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { discardSecret, provideSecret } from '@/api/client'
import {
  listSecretRequestsOptions,
  listSecretRequestsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { getErrorMessage } from '@/utils/errorHandling'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, Lock, ShieldCheck, X } from 'lucide-react'
import {
  useForm,
  type Resolver,
  type UseFormRegisterReturn,
} from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import {
  secretRequestState,
  type SecretRequest,
  type SecretRequestItem,
} from './chat-secrets'

type SecretForm = Record<string, string>

/** One non-empty value per input still waiting for the user. */
function schemaFor(ids: string[]) {
  return z.object(
    Object.fromEntries(
      ids.map((id) => [id, z.string().min(1, 'Enter the value to continue')])
    )
  )
}

/**
 * The assistant needs the credentials for a change it is about to propose. It
 * asks for all of them in one form, as the last step of the conversation, after
 * it has settled everything else with the user. The user types them here: they
 * go straight to Temps, are stored encrypted, and are never put in the
 * conversation. The assistant only receives opaque handles, which Temps swaps
 * for the values when the change you approve runs.
 */
export function SecretRequestCard({
  conversationPublicId,
  request,
  onProvided,
}: {
  conversationPublicId: string
  request: SecretRequest
  /** Called once every input of the form is saved, so the chat can continue. */
  onProvided?: () => void
}) {
  const queryClient = useQueryClient()
  const options = { path: { public_id: conversationPublicId } }
  const open = useQuery(listSecretRequestsOptions(options))
  const states = request.items.map((item) => ({
    item,
    state: secretRequestState(item.id, open.data),
  }))
  const awaiting = states
    .filter((s) => s.state === 'awaiting')
    .map((s) => s.item)
  const filled = states.filter((s) => s.state === 'filled').map((s) => s.item)
  const awaitingIds = awaiting.map((item) => item.id)

  // The schema follows the inputs that are still shown, so a value already
  // provided is not asked for again.
  const resolver: Resolver<SecretForm> = (values, context, resolverOptions) =>
    zodResolver(schemaFor(awaitingIds))(values, context, resolverOptions)
  const form = useForm<SecretForm>({ resolver, defaultValues: {} })
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: listSecretRequestsQueryKey(options),
    })

  const save = useMutation({
    mutationFn: async (values: SecretForm) => {
      // One by one, so a rejected value leaves the others saved and the form
      // can be submitted again for just the one that failed.
      for (const item of awaiting) {
        const { error } = await provideSecret({
          path: { public_id: conversationPublicId, secret_id: item.id },
          body: { value: values[item.id] ?? '' },
        })
        if (error) {
          void refresh()
          throw error
        }
        form.setValue(item.id, '')
      }
    },
    onSuccess: () => {
      // The values leave the form the moment they are stored.
      form.reset({})
      toast.success('Saved. Continuing…')
      void refresh()
      onProvided?.()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not save the values')),
  })
  const discard = useMutation({
    mutationFn: async (items: SecretRequestItem[]) => {
      for (const item of items) {
        const { error } = await discardSecret({
          path: { public_id: conversationPublicId, secret_id: item.id },
        })
        if (error) throw error
      }
    },
    onSuccess: () => {
      form.reset({})
      void refresh()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not discard the request')),
  })

  if (open.data === undefined) {
    return <div className="h-24 animate-pulse rounded-lg border bg-muted/30" />
  }
  if (awaiting.length === 0 && filled.length === 0) {
    return (
      <div className="flex items-center gap-2 rounded-lg border bg-muted/30 px-3 py-2 text-xs text-muted-foreground">
        <X className="size-4 shrink-0" />
        <span>
          {summaryOf(request)}: no longer available. Ask the assistant to
          request it again if it is still needed.
        </span>
      </div>
    )
  }
  if (awaiting.length === 0) {
    return (
      <div className="flex items-center gap-2 rounded-lg border border-emerald-500/30 bg-emerald-500/5 px-3 py-2 text-xs">
        <Check className="size-4 shrink-0 text-emerald-600 dark:text-emerald-300" />
        <span className="min-w-0 flex-1">
          {filled.map((item) => item.label).join(', ')}: provided by you. The
          assistant cannot see {filled.length === 1 ? 'it' : 'them'}.
        </span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs text-muted-foreground"
          disabled={discard.isPending}
          onClick={() => discard.mutate(filled)}
        >
          Discard
        </Button>
      </div>
    )
  }

  return (
    <form
      onSubmit={form.handleSubmit((values) => save.mutate(values))}
      className="min-w-0 space-y-4 rounded-lg border border-border bg-background p-3 sm:p-4"
      aria-label={`Provide the credentials for ${summaryOf(request)}`}
    >
      <div className="flex items-start gap-2">
        <Lock className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex flex-wrap items-center gap-2">
            <p className="text-sm font-medium">{summaryOf(request)}</p>
            <Badge variant="secondary" className="text-[10px] font-normal">
              Last step
            </Badge>
          </div>
          {request.purpose && (
            <p className="text-xs text-muted-foreground">{request.purpose}</p>
          )}
          <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
            <ShieldCheck className="mt-0.5 size-3.5 shrink-0" />
            Goes straight to Temps and is stored encrypted. It is not sent to
            the assistant or the AI provider, and it is used only for this
            change, in this chat, for 30 minutes.
          </p>
        </div>
      </div>

      {filled.map((item) => (
        <div
          key={item.id}
          className="flex items-center gap-2 rounded-md border border-emerald-500/30 bg-emerald-500/5 px-3 py-2 text-xs"
        >
          <Check className="size-4 shrink-0 text-emerald-600 dark:text-emerald-300" />
          <span className="min-w-0 flex-1 truncate">
            {item.label}: provided
          </span>
        </div>
      ))}

      {awaiting.map((item, index) => (
        <SecretInput
          key={item.id}
          item={item}
          autoFocus={index === 0}
          error={form.formState.errors[item.id]?.message}
          registration={form.register(item.id)}
        />
      ))}

      <div className="flex justify-end gap-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={discard.isPending || save.isPending}
          onClick={() => discard.mutate(awaiting)}
        >
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={save.isPending}>
          {save.isPending ? 'Saving…' : 'Save securely'}
        </Button>
      </div>
    </form>
  )
}

function summaryOf(request: SecretRequest): string {
  return (
    request.operationSummary?.replace(/\s*\(.*\)\s*$/, '') ??
    'Credentials for the assistant'
  )
}

function SecretInput({
  item,
  autoFocus,
  error,
  registration,
}: {
  item: SecretRequestItem
  autoFocus: boolean
  error: string | undefined
  registration: UseFormRegisterReturn
}) {
  const inputId = `secret-${item.id}`
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-baseline justify-between gap-x-2">
        <Label htmlFor={inputId} className="text-sm">
          {item.label}
        </Label>
        {item.field && (
          <code className="text-[11px] text-muted-foreground">
            {item.field}
          </code>
        )}
      </div>
      <Input
        id={inputId}
        type="password"
        autoComplete="off"
        spellCheck={false}
        autoFocus={autoFocus}
        placeholder={`Paste ${item.label.toLowerCase()}`}
        aria-invalid={error ? true : undefined}
        aria-describedby={item.help ? `${inputId}-help` : undefined}
        {...registration}
      />
      {item.help && (
        <p id={`${inputId}-help`} className="text-xs text-muted-foreground">
          {item.help}
        </p>
      )}
      {error && (
        <p className="text-xs text-destructive" role="alert">
          {error}
        </p>
      )}
    </div>
  )
}

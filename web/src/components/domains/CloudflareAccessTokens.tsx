// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Saved Cloudflare Access tokens (ADR 065 D9): the separate, account-wide
// token that lets a signed-in address on a tunnel sit behind Cloudflare
// Access. Shown on the DNS providers page, next to the Cloudflare accounts
// that run the tunnels.

import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  CheckCircle2,
  CircleHelp,
  ExternalLink,
  KeyRound,
  Loader2,
  Plus,
  ShieldCheck,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useLocation } from 'react-router'
import { toast } from 'sonner'
import type {
  AccessProviderResponse,
  AccessTokenCheckResult,
} from '@/api/client'
import {
  checkAccessProviderMutation,
  createAccessProviderMutation,
  deleteAccessProviderMutation,
  listAccessProvidersOptions,
  listAccessProvidersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useDebounce } from '@/hooks/useDebounce'
import { problemDetail } from '@/lib/api-problem'
import {
  ACCESS_SCOPE_NOTE,
  ACCESS_TOKEN_PERMISSIONS,
  accessTokenFormSchema,
  type AccessTokenFormValues,
  CLOUDFLARE_API_TOKENS_URL,
  deleteBlockedReason,
  isAccountId,
  keysLabel,
  protectsLabel,
} from '@/lib/cloudflare-access'
import { isForbiddenRead } from '@/lib/read-failure'
import { cn } from '@/lib/utils'

const permissionIcons = {
  present: CheckCircle2,
  missing: XCircle,
  unverified: CircleHelp,
} as const

const permissionTone = {
  present: 'text-emerald-700 dark:text-emerald-400',
  missing: 'text-destructive',
  unverified: 'text-amber-700 dark:text-amber-400',
} as const

/** Each Access permission a checked token has or lacks. */
function AccessPermissionList({ result }: { result: AccessTokenCheckResult }) {
  if (!result.valid)
    return (
      <p role="status" className="text-xs text-destructive">
        {result.error ?? 'Cloudflare did not accept this token.'}
      </p>
    )
  return (
    <ul role="status" className="space-y-1 text-xs">
      {result.permissions.map((check) => {
        const Icon = permissionIcons[check.state]
        return (
          <li key={check.permission} className="flex gap-2">
            <Icon
              aria-hidden="true"
              className={cn(
                'mt-0.5 size-3.5 shrink-0',
                permissionTone[check.state]
              )}
            />
            <span>
              <code className="rounded bg-muted px-1">{check.label}</code>{' '}
              <span className="sr-only">{check.state}</span>
              {check.state !== 'present' && (
                <span className="text-muted-foreground">{check.detail}</span>
              )}
            </span>
          </li>
        )
      })}
      {result.team_domain && (
        <li className="text-muted-foreground">
          Access team: <code>{result.team_domain}</code>
        </li>
      )}
    </ul>
  )
}

/** Checks a pasted token as soon as it and the account id are there. */
function AccessTokenCheck({
  token,
  accountId,
}: {
  token: string
  accountId: string
}) {
  const pasted = useDebounce(token.trim(), 400)
  const account = useDebounce(accountId.trim(), 400)
  const check = useMutation(checkAccessProviderMutation())
  const { mutate } = check
  const ready = pasted.length >= 20 && isAccountId(account)
  useEffect(() => {
    if (ready) mutate({ body: { token: pasted, account_id: account } })
  }, [ready, pasted, account, mutate])
  if (!ready) return null
  if (check.isPending) return <Skeleton className="h-12 w-full" />
  if (check.isError)
    return (
      <p role="status" className="text-xs text-muted-foreground">
        Temps could not check the token now; it is checked again when you save
        it.
      </p>
    )
  if (!check.data) return null
  return <AccessPermissionList result={check.data} />
}

function AccessTokenChecklist() {
  return (
    <div className="space-y-2 rounded-md border bg-muted/30 p-3 text-xs">
      <p className="text-sm font-medium">The token needs</p>
      <ul className="space-y-1">
        {ACCESS_TOKEN_PERMISSIONS.map((entry) => (
          <li key={entry.permission} className="flex flex-wrap gap-x-2">
            <code className="rounded bg-muted px-1">{entry.permission}</code>
            <span className="text-muted-foreground">to {entry.purpose}</span>
          </li>
        ))}
      </ul>
      <p className="text-amber-700 dark:text-amber-400">{ACCESS_SCOPE_NOTE}</p>
      <a
        className="inline-flex items-center gap-0.5 text-foreground underline"
        href={CLOUDFLARE_API_TOKENS_URL}
        target="_blank"
        rel="noreferrer"
      >
        Create it in Cloudflare
        <ExternalLink className="h-3 w-3" />
      </a>
    </div>
  )
}

function AddAccessTokenForm({ onDone }: { onDone: () => void }) {
  const queryClient = useQueryClient()
  const form = useForm<AccessTokenFormValues>({
    resolver: zodResolver(accessTokenFormSchema),
    defaultValues: { name: 'Cloudflare Access', account_id: '', token: '' },
  })
  const [token, accountId] = useWatch({
    control: form.control,
    name: ['token', 'account_id'],
  })
  const create = useMutation({
    ...createAccessProviderMutation(),
    onSuccess: (saved) => {
      toast.success(`Saved ${saved.name}`, {
        description: `Access team ${saved.team_domain}`,
      })
      void queryClient.invalidateQueries({
        queryKey: listAccessProvidersQueryKey(),
      })
      form.reset()
      onDone()
    },
    onError: (error) =>
      toast.error('The token was not saved', {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  return (
    <Form {...form}>
      <form
        className="space-y-4"
        onSubmit={form.handleSubmit((values) =>
          create.mutate({ body: values })
        )}
      >
        <AccessTokenChecklist />
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <FormField
            control={form.control}
            name="name"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Name</FormLabel>
                <FormControl>
                  <Input autoComplete="off" {...field} />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="account_id"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Cloudflare account id</FormLabel>
                <FormControl>
                  <Input
                    className="font-mono"
                    autoComplete="off"
                    placeholder="32 characters"
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
        </div>
        <FormField
          control={form.control}
          name="token"
          render={({ field }) => (
            <FormItem>
              <FormLabel>API token</FormLabel>
              <FormControl>
                <Input
                  type="password"
                  className="font-mono"
                  autoComplete="off"
                  {...field}
                />
              </FormControl>
              <FormDescription>
                Checked before it is saved, then stored encrypted. Temps never
                shows it again.
              </FormDescription>
              <FormMessage />
            </FormItem>
          )}
        />
        <AccessTokenCheck token={token ?? ''} accountId={accountId ?? ''} />
        <div className="flex flex-wrap gap-2">
          <Button type="submit" disabled={create.isPending}>
            {create.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Check and save
          </Button>
          <Button type="button" variant="ghost" onClick={onDone}>
            Cancel
          </Button>
        </div>
      </form>
    </Form>
  )
}

export function SavedAccessToken({
  provider,
  canManage,
  onDelete,
}: {
  provider: AccessProviderResponse
  canManage: boolean
  onDelete: (provider: AccessProviderResponse) => void
}) {
  const blocked = deleteBlockedReason(provider.addresses)
  // Visible, not only a tooltip: a disabled button takes no focus and touch
  // screens show no `title`, yet the reason names the step that unblocks it.
  const blockedId = `access-token-${provider.id}-delete-blocked`
  const showBlocked = canManage && blocked !== null
  return (
    <li className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center">
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
          <KeyRound className="h-4 w-4" aria-hidden="true" />
        </div>
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">{provider.name}</p>
          <p className="truncate text-xs text-muted-foreground">
            <code>{provider.team_domain}</code> ·{' '}
            {protectsLabel(provider.addresses)} ·{' '}
            {keysLabel(provider.key_count, provider.keys_fetched_at)} · added{' '}
            <TimeAgo date={provider.created_at} />
          </p>
          {showBlocked && (
            <p id={blockedId} className="text-xs text-muted-foreground">
              {blocked}
            </p>
          )}
        </div>
      </div>
      {canManage && (
        <Button
          variant="outline"
          size="sm"
          disabled={blocked !== null}
          aria-describedby={showBlocked ? blockedId : undefined}
          onClick={() => onDelete(provider)}
        >
          <Trash2 className="h-4 w-4 sm:mr-2" aria-hidden="true" />
          <span className="max-sm:sr-only">Delete</span>
        </Button>
      )}
    </li>
  )
}

/**
 * The saved Access tokens, and adding one. Always shown: with none saved it
 * says what Access adds and how to set it up (CLAUDE.md, discoverability).
 */
export function CloudflareAccessTokens({ canManage }: { canManage: boolean }) {
  const queryClient = useQueryClient()
  const [adding, setAdding] = useState(false)
  const [toDelete, setToDelete] = useState<AccessProviderResponse | null>(null)
  const providers = useQuery(listAccessProvidersOptions())
  // A plan blocked on a missing token links here (ADR 065 D9).
  const { hash } = useLocation()
  const loaded = providers.isSuccess
  useEffect(() => {
    if (hash === '#cloudflare-access')
      document
        .getElementById('cloudflare-access')
        ?.scrollIntoView({ block: 'start' })
  }, [hash, loaded])
  const remove = useMutation({
    ...deleteAccessProviderMutation(),
    onSuccess: () => {
      toast.success(`Deleted ${toDelete?.name ?? 'the token'}`)
      setToDelete(null)
      void queryClient.invalidateQueries({
        queryKey: listAccessProvidersQueryKey(),
      })
    },
    onError: (error) =>
      toast.error('The token was not deleted', {
        description: problemDetail(error, 'Try again in a moment.'),
      }),
  })
  const saved = providers.data ?? []
  return (
    <Card id="cloudflare-access">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <ShieldCheck className="h-4 w-4" aria-hidden="true" />
          Cloudflare Access
        </CardTitle>
        <CardDescription>
          Put Cloudflare Access in front of an address that goes through a
          Cloudflare Tunnel, for example to let people in with your company
          identity provider. Choose it under &ldquo;For people who sign
          in&rdquo; when making an address reachable. It needs its own
          Cloudflare token.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {providers.error ? (
          <ReadFailure
            resource="Cloudflare Access tokens"
            error={providers.error}
            onRetry={
              isForbiddenRead(providers.error)
                ? undefined
                : () => void providers.refetch()
            }
            retrying={providers.isFetching}
          />
        ) : providers.isLoading ? (
          <Skeleton className="h-14 w-full" />
        ) : saved.length > 0 ? (
          <ul role="list" className="divide-y rounded-lg border">
            {saved.map((provider) => (
              <SavedAccessToken
                key={provider.id}
                provider={provider}
                canManage={canManage}
                onDelete={setToDelete}
              />
            ))}
          </ul>
        ) : (
          <p className="text-sm text-muted-foreground">
            No Access token is saved, so signed-in addresses use the Temps
            sign-in.
          </p>
        )}
        {canManage &&
          (adding ? (
            <AddAccessTokenForm onDone={() => setAdding(false)} />
          ) : (
            <Button variant="outline" size="sm" onClick={() => setAdding(true)}>
              <Plus className="mr-2 h-4 w-4" aria-hidden="true" />
              Add an Access token
            </Button>
          ))}
      </CardContent>
      <ConfirmDeleteDialog
        open={canManage && toDelete !== null}
        onOpenChange={(open) => {
          if (!open) setToDelete(null)
        }}
        resourceType="Cloudflare Access token"
        resourceName={toDelete?.name ?? ''}
        confirmLabel="Delete token"
        consequences="Temps forgets the token. Access apps stay in Cloudflare as they are."
        isPending={remove.isPending}
        onConfirm={() => {
          if (toDelete) remove.mutate({ path: { provider_id: toDelete.id } })
        }}
      />
    </Card>
  )
}

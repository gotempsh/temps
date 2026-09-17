// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect } from 'react'
import { useForm, Controller } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { getPluginGrants, putPluginGrants } from '@/api/client/sdk.gen'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { PluginGrantFields } from './PluginGrantFields'
import {
  emptyPluginGrants,
  pluginGrantsSchema,
  type PluginGrantsValues,
} from '@/lib/plugin-grants'
import { z } from 'zod'
import { toast } from 'sonner'
import { Link } from 'react-router'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'

const schema = z.object({ grants: pluginGrantsSchema })

export function PluginPermissionsDialog({
  name,
  open,
  onOpenChange,
  onSensitiveError,
}: {
  name: string | null
  open: boolean
  onOpenChange: (open: boolean) => void
  onSensitiveError: (error: unknown, retry: () => void) => boolean
}) {
  const queries = useQueryClient()
  const key = ['external-plugin-grants', name]
  const grants = useQuery({
    queryKey: key,
    queryFn: async () => {
      if (!name) throw new Error('Select a plugin to view its permissions.')
      return (await getPluginGrants({ path: { name }, throwOnError: true }))
        .data
    },
    enabled: open && name !== null,
    retry: false,
    staleTime: 0,
    refetchOnWindowFocus: false,
  })
  const form = useForm<{ grants: PluginGrantsValues }>({
    resolver: zodResolver(schema),
    defaultValues: { grants: emptyPluginGrants() },
  })
  const { reset } = form
  useEffect(() => {
    if (open && grants.data)
      reset({
        grants: {
          permissions: grants.data.permissions,
          ai_daily_call_limit: grants.data.ai.daily_call_limit,
          ai_max_output_tokens: grants.data.ai.max_output_tokens,
        },
      })
    else reset({ grants: emptyPluginGrants() })
  }, [grants.data, open, reset])
  const save = useMutation({
    mutationFn: async (values: PluginGrantsValues) => {
      if (!name) throw new Error('Select a plugin before changing permissions.')
      return (
        await putPluginGrants({
          path: { name },
          body: values,
          throwOnError: true,
        })
      ).data
    },
    onSuccess: async () => {
      await queries.invalidateQueries({ queryKey: key })
      toast.success(
        'Plugin permissions updated. New calls use these grants immediately.'
      )
      onOpenChange(false)
    },
  })
  async function submit(values: PluginGrantsValues) {
    try {
      await save.mutateAsync(values)
    } catch (error) {
      if (onSensitiveError(error, () => void submit(values))) return
      toast.error(
        sensitiveActionErrorMessage(
          error,
          'Could not update plugin permissions. Your changes have not been saved.'
        )
      )
    }
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!save.isPending) onOpenChange(next)
      }}
    >
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Permissions for {name}</DialogTitle>
          <DialogDescription>
            Control this plugin’s access to Temps. Changes apply to subsequent
            calls without a restart.
          </DialogDescription>
        </DialogHeader>
        {grants.isPending ? (
          <div
            role="status"
            aria-label="Loading plugin permissions"
            className="space-y-3"
          >
            {[0, 1, 2].map((row) => (
              <Skeleton key={row} className="h-16 w-full" />
            ))}
          </div>
        ) : grants.isError ? (
          <div role="alert" className="space-y-3 text-sm">
            <p>
              Could not load permissions. Nothing can be changed until current
              grants are available.
            </p>
            <Button variant="outline" onClick={() => void grants.refetch()}>
              Retry
            </Button>
          </div>
        ) : grants.data ? (
          <form
            onSubmit={form.handleSubmit(({ grants: values }) => submit(values))}
            className="space-y-4"
          >
            <Controller
              control={form.control}
              name="grants"
              render={({ field }) => (
                <PluginGrantFields
                  value={field.value}
                  onChange={field.onChange}
                  disabled={save.isPending || grants.isFetching}
                  requested={grants.data.requested_permissions}
                />
              )}
            />
            {!grants.data.ai.configured && (
              <p className="text-sm text-muted-foreground">
                {grants.data.ai.reason || 'No AI provider is configured.'}{' '}
                <Link className="underline" to="/settings/ai-providers">
                  Configure AI
                </Link>
              </p>
            )}
            {form.formState.errors.grants && (
              <p role="alert" className="text-sm text-destructive">
                Choose 0–10,000 daily AI calls and 1–4,096 output tokens.
              </p>
            )}
            <p className="break-all text-xs text-muted-foreground">
              Audit actor: {grants.data.actor.id}
            </p>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                disabled={save.isPending}
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={save.isPending || grants.isFetching}
              >
                {save.isPending ? 'Saving…' : 'Save permissions'}
              </Button>
            </DialogFooter>
          </form>
        ) : null}
      </DialogContent>
    </Dialog>
  )
}

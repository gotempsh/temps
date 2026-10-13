// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, ArrowUpCircle, Loader2, RefreshCw } from 'lucide-react'
import { toast } from 'sonner'

import {
  getAiProviderHarnessUpdate,
  refreshAiProviderModels,
  updateAiProviderHarness,
  type ProviderCatalogResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { aiProviderCatalogQueryOptions } from '@/lib/ai-provider-catalog-query'
import { mergeProviderModelRefresh } from '@/pages/agent-sandbox/provider-model-catalog'
import { problemDetail } from '@/lib/api-problem'
import {
  HARNESS_UPDATE_STALE_MS,
  harnessUpdateCheckProblem,
  harnessUpdateDetail,
  harnessUpdateHeadline,
  harnessUpdateRefetchInterval,
  shouldShowHarnessUpdate,
} from './harness-update'

/**
 * Tells the operator the harness CLI in their workspace is behind the latest
 * release and offers to update it. The workspace image bundles one release, so
 * it drifts and then stops offering newer models; nothing is updated without
 * the operator pressing the button.
 */
export function HarnessUpdateBar({
  providerId,
  providerName,
  enabled,
}: {
  providerId: string
  providerName: string
  /** Only look inside the workspace once the harness has a saved connection. */
  enabled: boolean
}) {
  const queryClient = useQueryClient()
  const queryKey = ['harness-update', providerId] as const

  const status = useQuery({
    queryKey,
    queryFn: async () => {
      const { data } = await getAiProviderHarnessUpdate({
        path: { provider_id: providerId },
        throwOnError: true,
      })
      return data
    },
    enabled,
    staleTime: HARNESS_UPDATE_STALE_MS,
    retry: false,
    refetchOnWindowFocus: false,
    refetchInterval: (query) => harnessUpdateRefetchInterval(query.state.data),
  })

  // An explicit retry looks inside the workspace again: the server keeps a
  // failed check for hours, so a plain refetch would return the same failure.
  const recheck = useMutation({
    mutationFn: async () => {
      const { data } = await getAiProviderHarnessUpdate({
        path: { provider_id: providerId },
        query: { refresh: true },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (next) => queryClient.setQueryData(queryKey, next),
  })

  const update = useMutation({
    mutationFn: async () => {
      const { data } = await updateAiProviderHarness({
        path: { provider_id: providerId },
        throwOnError: true,
      })
      return data
    },
    onMutate: () => {
      // Show "Updating…" immediately; the poll keeps it current.
      queryClient.setQueryData(queryKey, (current: typeof status.data) =>
        current ? { ...current, updating: true } : current
      )
    },
    onSuccess: async (next) => {
      queryClient.setQueryData(queryKey, next)
      toast.success(`${providerName} updated`, {
        description: next.installed_version
          ? `Now on ${next.installed_version}. Reloading the model list.`
          : 'Reloading the model list.',
      })
      try {
        const refreshed = await refreshAiProviderModels({
          path: { provider_id: providerId },
          throwOnError: true,
        })
        queryClient.setQueryData<ProviderCatalogResponse>(
          aiProviderCatalogQueryOptions.queryKey,
          (catalog) =>
            catalog
              ? mergeProviderModelRefresh(catalog, refreshed.data)
              : catalog
        )
      } catch {
        // The picker offers its own retry; the update itself succeeded.
        void queryClient.invalidateQueries({
          queryKey: aiProviderCatalogQueryOptions.queryKey,
        })
      }
    },
    onError: (error) => {
      void queryClient.invalidateQueries({ queryKey })
      toast.error(`Could not update ${providerName}`, {
        description: problemDetail(
          error,
          'The update did not finish. Check the workspace and try again.'
        ),
      })
    },
  })

  const checkProblem = recheck.isError
    ? problemDetail(
        recheck.error,
        `Could not check whether ${providerName} has an update.`
      )
    : status.isError
      ? problemDetail(
          status.error,
          `Could not check whether ${providerName} has an update.`
        )
      : harnessUpdateCheckProblem(status.data, providerName)
  if (checkProblem) {
    return (
      <div
        role="status"
        aria-live="polite"
        className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2"
      >
        <div className="flex min-w-0 items-start gap-2 text-sm">
          <AlertTriangle
            className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-300"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <p className="font-medium">
              Couldn&apos;t check for a {providerName} update
            </p>
            <p className="text-xs break-words text-muted-foreground">
              {checkProblem} New models won&apos;t be offered until the CLI in
              your workspace is current.
            </p>
          </div>
        </div>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={status.isFetching || recheck.isPending}
          onClick={() => recheck.mutate()}
        >
          {status.isFetching || recheck.isPending ? (
            <Loader2 className="mr-1.5 size-4 animate-spin" />
          ) : (
            <RefreshCw className="mr-1.5 size-4" />
          )}
          Check again
        </Button>
      </div>
    )
  }
  if (!shouldShowHarnessUpdate(status.data) || !status.data) return null
  const running = status.data.updating || update.isPending

  return (
    <div
      role="status"
      aria-live="polite"
      className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-sky-500/30 bg-sky-500/5 px-3 py-2"
    >
      <div className="min-w-0 text-sm">
        <p className="font-medium">
          {harnessUpdateHeadline(providerName, {
            ...status.data,
            updating: running,
          })}
        </p>
        <p className="text-xs text-muted-foreground">
          {harnessUpdateDetail(providerName, {
            ...status.data,
            updating: running,
          })}
        </p>
      </div>
      <Button
        type="button"
        size="sm"
        disabled={running}
        onClick={() => update.mutate()}
      >
        {running ? (
          <Loader2 className="mr-1.5 size-4 animate-spin" />
        ) : (
          <ArrowUpCircle className="mr-1.5 size-4" />
        )}
        {running ? 'Updating…' : 'Update now'}
      </Button>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getPluginStatus, updateRepository } from '@/api/client/sdk.gen'
import { useForm } from 'react-hook-form'
import { useEffect } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { repositoryInstallSchema } from '@/lib/plugin-repository'
import { Button } from '@/components/ui/button'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { toast } from 'sonner'
import {
  GitBranch,
  GitCommitHorizontal,
  Code2,
  Folder,
  Download,
} from 'lucide-react'

function useRepositorySource(name: string) {
  const statusKey = [...PLUGINS_QUERY_KEY, name, 'source']
  return useQuery({
    queryKey: statusKey,
    queryFn: async () => {
      const response = await getPluginStatus({
        path: { name },
        throwOnError: true,
      })
      return response.data
    },
    enabled: Boolean(name),
    retry: false,
    staleTime: 60_000,
  })
}

export function RepositoryUpdateButton({
  name,
  disabled,
  onClick,
}: {
  name: string
  disabled: boolean
  onClick: () => void
}) {
  const status = useRepositorySource(name)
  if (status.isPending) return <Skeleton className="h-8 w-24" />
  if (status.isSuccess && !status.data?.source)
    return (
      <span className="text-xs text-muted-foreground">
        Manual update · no GitHub source
      </span>
    )
  return (
    <Button variant="outline" size="sm" onClick={onClick} disabled={disabled}>
      <Download className="mr-2 size-4" />
      {status.isError ? 'Retry update source' : 'Update'}
    </Button>
  )
}

export function RepositoryUpdate({
  name,
  disabled,
  onSensitiveError,
  onPendingChange,
}: {
  onPendingChange?: (pending: boolean) => void
  name: string
  disabled: boolean
  onSensitiveError: (error: unknown, retry: () => void) => boolean
}) {
  const queries = useQueryClient()
  const form = useForm<{ ref_name?: string }>({
    resolver: zodResolver(repositoryInstallSchema.pick({ ref_name: true })),
    defaultValues: { ref_name: '' },
  })
  const status = useRepositorySource(name)
  const { reset } = form
  useEffect(() => {
    reset({ ref_name: '' })
  }, [reset, status.data?.source?.ref_name, status.data?.source?.commit])
  const update = useMutation({
    mutationFn: async (ref: string) => {
      const response = await updateRepository({
        path: { name },
        body: ref ? { ref_name: ref } : {},
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: async (result) => {
      reset({ ref_name: '' })
      toast.success(result.message)
      await queries.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
  useEffect(() => {
    onPendingChange?.(update.isPending)
  }, [update.isPending, onPendingChange])
  async function runUpdate(ref: string) {
    try {
      await update.mutateAsync(ref)
    } catch (error) {
      if (onSensitiveError(error, () => void runUpdate(ref))) return
      toast.error(
        sensitiveActionErrorMessage(
          error,
          'Update failed. The previous plugin remains installed.'
        )
      )
    }
  }
  if (status.isError)
    return (
      <span className="text-xs text-destructive">
        Could not load update source.
      </span>
    )
  if (!name) return null
  if (status.isPending)
    return (
      <div
        role="status"
        aria-label="Loading plugin source"
        className="space-y-5"
      >
        <div aria-hidden="true" className="space-y-3 rounded-md border p-3">
          <Skeleton className="h-5 w-3/4" />
          <Skeleton className="h-4 w-1/2" />
        </div>
        <div aria-hidden="true" className="space-y-1">
          <Skeleton className="h-4 w-40" />
          <Skeleton className="h-9 w-full" />
        </div>
        <Skeleton aria-hidden="true" className="h-8 w-44" />
      </div>
    )
  if (!status.data?.source)
    return <p>This plugin has no GitHub source available for updates.</p>
  const source = status.data.source
  return (
    <form
      onSubmit={form.handleSubmit((values) => runUpdate(values.ref_name || ''))}
      className="space-y-5"
    >
      <div className="space-y-3 rounded-md border bg-muted/20 p-3 text-sm">
        <p className="flex items-start gap-2 break-all">
          <Code2 className="mt-0.5 size-4 shrink-0" />
          {source.repository_url.replace('https://github.com/', '')}
        </p>
        <div className="flex flex-wrap gap-3 text-xs text-muted-foreground">
          {!/^[a-f0-9]{40}$/i.test(source.ref_name) && (
            <span className="flex items-center gap-1">
              <GitBranch className="size-3.5" />
              {source.ref_name}
            </span>
          )}
          <span className="flex items-center gap-1" title={source.commit}>
            <GitCommitHorizontal className="size-3.5" />
            {source.commit.slice(0, 8)}
          </span>
          {source.path && (
            <span className="flex items-center gap-1">
              <Folder className="size-3.5" />
              {source.path}
            </span>
          )}
        </div>
      </div>
      <div className="space-y-1">
        <Label htmlFor={`plugin-update-ref-${name}`} className="text-xs">
          Update branch, tag, or commit
        </Label>
        <Input
          id={`plugin-update-ref-${name}`}
          {...form.register('ref_name')}
          placeholder={`Keep ${/^[a-f0-9]{40}$/i.test(source.ref_name) ? source.ref_name.slice(0, 8) : source.ref_name}`}
          disabled={disabled || update.isPending}
          aria-invalid={Boolean(form.formState.errors.ref_name)}
          className="w-full"
        />
        {form.formState.errors.ref_name && (
          <p role="alert" className="text-xs text-destructive">
            Enter a valid branch, tag, or commit.
          </p>
        )}
      </div>
      <Button
        variant="outline"
        size="sm"
        disabled={disabled || update.isPending}
        type="submit"
      >
        <Download className="mr-2 size-4" />
        {update.isPending ? 'Building update…' : 'Update from GitHub'}
      </Button>
    </form>
  )
}

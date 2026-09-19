// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { getPluginStatus, updateRepository } from '@/api/client/sdk.gen'
import { useForm } from 'react-hook-form'
import { useEffect } from 'react'
import { zodResolver } from '@hookform/resolvers/zod'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { repositoryInstallSchema } from '@/lib/plugin-repository'
import { Button } from '@/components/ui/button'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { toast } from 'sonner'

export function RepositoryUpdate({
  name,
  disabled,
  onSensitiveError,
}: {
  name: string
  disabled: boolean
  onSensitiveError: (error: unknown, retry: () => void) => boolean
}) {
  const queries = useQueryClient()
  const form = useForm<{ ref_name?: string }>({
    resolver: zodResolver(repositoryInstallSchema.pick({ ref_name: true })),
    defaultValues: { ref_name: '' },
  })
  const statusKey = [...PLUGINS_QUERY_KEY, name, 'source']
  const status = useQuery({
    queryKey: statusKey,
    queryFn: async () => {
      const response = await getPluginStatus({
        path: { name },
        throwOnError: true,
      })
      return response.data
    },
    retry: false,
    staleTime: 60_000,
  })
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
  if (!status.data?.source) return null
  const source = status.data.source
  return (
    <form
      onSubmit={form.handleSubmit((values) => runUpdate(values.ref_name || ''))}
      className="flex flex-wrap items-center gap-2"
    >
      <span
        className="text-xs text-muted-foreground"
        title={`${source.repository_url} · ${source.path || 'Repository root'} · ${source.ref_name} · ${source.commit}`}
      >
        {source.path && <>{source.path} · </>}
        {source.ref_name} · {source.commit.slice(0, 8)}
      </span>
      <div className="space-y-1">
        <Label htmlFor={`plugin-update-ref-${name}`} className="text-xs">
          Update branch, tag, or commit
        </Label>
        <Input
          id={`plugin-update-ref-${name}`}
          {...form.register('ref_name')}
          placeholder={`Keep ${source.ref_name}`}
          disabled={disabled || update.isPending}
          aria-invalid={Boolean(form.formState.errors.ref_name)}
          className="h-8 w-56"
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
        {update.isPending ? 'Building update…' : 'Update from GitHub'}
      </Button>
    </form>
  )
}

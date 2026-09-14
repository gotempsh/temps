// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'
import { Button } from '@/components/ui/button'
import { PLUGINS_QUERY_KEY } from '@/hooks/usePlugins'
import { sensitiveActionErrorMessage } from '@/lib/sensitiveActionProblem'
import { toast } from 'sonner'

type Source = { repository_url: string; ref_name: string; commit: string }
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
  const statusKey = [...PLUGINS_QUERY_KEY, name, 'source']
  const status = useQuery({
    queryKey: statusKey,
    queryFn: async () => {
      const response = await client.get<
        { 200: { source?: Source } },
        unknown,
        true
      >({
        url: `/x/plugins/${encodeURIComponent(name)}/status`,
        throwOnError: true,
      })
      return response.data
    },
    retry: false,
    staleTime: 60_000,
  })
  const update = useMutation({
    mutationFn: async () => {
      const response = await client.post<
        { 200: { message: string } },
        unknown,
        true
      >({
        url: `/x/plugins/${encodeURIComponent(name)}/update`,
        body: {},
        headers: { 'Content-Type': 'application/json' },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: async (result) => {
      toast.success(result.message)
      await queries.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
  async function runUpdate() {
    try {
      await update.mutateAsync()
    } catch (error) {
      if (onSensitiveError(error, () => void runUpdate())) return
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
    <div className="flex flex-wrap items-center gap-2">
      <span
        className="text-xs text-muted-foreground"
        title={`${source.repository_url} · ${source.ref_name} · ${source.commit}`}
      >
        {source.ref_name} · {source.commit.slice(0, 8)}
      </span>
      <Button
        variant="outline"
        size="sm"
        disabled={disabled || update.isPending}
        onClick={() => void runUpdate()}
      >
        {update.isPending ? 'Building update…' : 'Update from GitHub'}
      </Button>
    </div>
  )
}

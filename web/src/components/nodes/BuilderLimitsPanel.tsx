// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Button, Field, SettingsGroup } from '@temps-sdk/ds'
import { Input } from '@/components/ui/input'
import {
  adminGetNodeOptions,
  adminSetNodeBuildLimitsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { NodeInfoResponse } from '@/api/client/types.gen'
import { problemDetail } from '@/lib/api-problem'
import { toast } from 'sonner'

const limitsSchema = z.object({
  concurrency: z.number().int().min(1).max(32),
  queue_wait_seconds: z.number().int().min(1).max(1800),
  build_timeout_seconds: z.number().int().min(1).max(14400),
})

export function BuilderLimitsPanel({ node }: { node: NodeInfoResponse }) {
  const form = useForm<z.infer<typeof limitsSchema>>({
    resolver: zodResolver(limitsSchema),
    defaultValues: node.build_limits,
  })
  const queryClient = useQueryClient()
  const update = useMutation({
    ...adminSetNodeBuildLimitsMutation(),
    onSuccess: async () => {
      toast.success('Builder limits saved')
      await queryClient.invalidateQueries({
        queryKey: adminGetNodeOptions({ path: { node_id: node.id } }).queryKey,
      })
    },
  })
  return (
    <SettingsGroup
      title="Builder limits"
      description="Limit concurrent builds and waiting time on this worker. Upgrade older agents to apply custom limits."
    >
      <form
        className="space-y-4"
        onSubmit={form.handleSubmit((limits) => {
          if (!update.isPending)
            update.mutate({ path: { node_id: node.id }, body: limits })
        })}
      >
        {(
          [
            ['concurrency', 'Concurrent builds', 1, 32],
            ['queue_wait_seconds', 'Queue wait (seconds)', 1, 1800],
            ['build_timeout_seconds', 'Build timeout (seconds)', 1, 14400],
          ] as const
        ).map(([key, label, min, max]) => (
          <Field
            key={key}
            label={label}
            description={`${min}–${max}`}
            error={form.formState.errors[key]?.message}
          >
            {(props) => (
              <Input
                {...props}
                readOnly={update.isPending}
                {...form.register(key, { valueAsNumber: true })}
                required
                type="number"
                min={min}
                max={max}
                step={1}
              />
            )}
          </Field>
        ))}
        <p className="text-sm text-muted-foreground">
          When the queue wait expires, a node-preferred build falls back to the
          control plane if local builds are enabled. A control plane without
          Docker fails with the builder’s reason.
        </p>
        {update.isError && (
          <p role="alert" className="text-sm text-destructive">
            {problemDetail(
              update.error,
              'Could not save builder limits. Try again.'
            )}
          </p>
        )}
        <Button type="submit" busy={update.isPending} busyLabel="Saving…">
          Save builder limits
        </Button>
      </form>
    </SettingsGroup>
  )
}

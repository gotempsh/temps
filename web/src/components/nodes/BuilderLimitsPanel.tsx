// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Button, Field } from '@temps-sdk/ds'
import { Hammer } from 'lucide-react'
import { Card, CardContent } from '@/components/ui/card'
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

/** 300 → "5 min", 5400 → "1 h 30 min". */
function duration(seconds: number) {
  if (seconds < 60) return `${seconds} s`
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes} min`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest ? `${hours} h ${rest} min` : `${hours} h`
}

export function BuilderLimitsPanel({ node }: { node: NodeInfoResponse }) {
  // The server's defaults, for fields an older control plane leaves out.
  const limits = {
    concurrency: node.build_limits.concurrency ?? 1,
    queue_wait_seconds: node.build_limits.queue_wait_seconds ?? 300,
    build_timeout_seconds: node.build_limits.build_timeout_seconds ?? 1800,
  }
  const form = useForm<z.infer<typeof limitsSchema>>({
    resolver: zodResolver(limitsSchema),
    defaultValues: limits,
  })
  const queryClient = useQueryClient()
  const update = useMutation({
    ...adminSetNodeBuildLimitsMutation(),
    onSuccess: async (_, { body }) => {
      toast.success('Builder limits saved')
      form.reset(body)
      await queryClient.invalidateQueries({
        queryKey: adminGetNodeOptions({ path: { node_id: node.id } }).queryKey,
      })
    },
  })
  const dirty = form.formState.isDirty
  return (
    <Card className="shadow-none">
      <CardContent className="space-y-4 p-4">
        <div className="space-y-1">
          <div className="flex items-center gap-2 text-sm font-medium">
            <Hammer className="h-4 w-4" /> Builds on this worker
          </div>
          <p className="text-sm text-muted-foreground">
            Now: up to {limits.concurrency} build
            {limits.concurrency === 1 ? '' : 's'} at a time. A build waits up to{' '}
            {duration(limits.queue_wait_seconds)} for a slot and stops after{' '}
            {duration(limits.build_timeout_seconds)}.
          </p>
        </div>

        <form
          className="space-y-4"
          onSubmit={form.handleSubmit((next) => {
            if (!update.isPending)
              update.mutate({ path: { node_id: node.id }, body: next })
          })}
        >
          <div className="grid gap-4 md:grid-cols-3">
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
          </div>
          <p className="text-xs text-muted-foreground">
            When the queue wait expires, a node-preferred build falls back to
            the control plane if local builds are enabled. A control plane
            without Docker fails with the builder’s reason. Older agents apply
            custom limits only after an upgrade.
          </p>
          {update.isError && (
            <p role="alert" className="text-sm text-destructive">
              {problemDetail(
                update.error,
                'Could not save builder limits. Try again.'
              )}
            </p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button
              type="submit"
              disabled={!dirty}
              busy={update.isPending}
              busyLabel="Saving…"
            >
              Save builder limits
            </Button>
            {dirty && !update.isPending && (
              <Button
                type="button"
                variant="outline"
                onClick={() => {
                  update.reset()
                  form.reset(limits)
                }}
              >
                Reset
              </Button>
            )}
          </div>
        </form>
      </CardContent>
    </Card>
  )
}

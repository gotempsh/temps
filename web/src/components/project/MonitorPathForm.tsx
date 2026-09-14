// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MonitorResponse } from '@/api/client'
import {
  getMonitorOptions,
  listMonitorsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { updateMonitorPath } from '@/api/monitors'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  check_path: z
    .string()
    .startsWith('/', 'Enter a path starting with /, such as /health.')
    .refine(
      (path) => new TextEncoder().encode(path).length <= 2048,
      'The path must be at most 2048 bytes.'
    )
    .refine(
      (path) =>
        !path.includes('://') &&
        !/[@?#\s\\]/.test(path) &&
        !path.startsWith('//'),
      'Use a path without a hostname, spaces, query parameters, or fragments.'
    ),
})

export function MonitorPathForm({ monitor }: { monitor: MonitorResponse }) {
  const queryClient = useQueryClient()
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: {
      check_path:
        monitor.check_path ||
        (monitor.monitor_type === 'health' ? '/health' : '/'),
    },
  })
  const mutation = useMutation({
    mutationFn: (values: z.infer<typeof schema>) =>
      updateMonitorPath(monitor.id, values.check_path),
    onSuccess: (updated) => {
      queryClient.setQueryData(
        getMonitorOptions({ path: { monitor_id: monitor.id } }).queryKey,
        updated
      )
      void queryClient.invalidateQueries({
        queryKey: listMonitorsQueryKey({
          path: { project_id: monitor.project_id },
        }),
      })
      form.reset({ check_path: updated.check_path || '/' })
      toast.success('Monitor path updated')
    },
    onError: () => {
      form.setError('root', {
        message:
          'Could not save the monitor path. Check your permissions and try again.',
      })
    },
  })

  return (
    <Form {...form}>
      <form
        className="max-w-xl space-y-3 pt-3"
        onSubmit={form.handleSubmit((values) => mutation.mutate(values))}
      >
        <FormField
          control={form.control}
          name="check_path"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Monitor path</FormLabel>
              <FormControl>
                <Input
                  {...field}
                  placeholder="/health"
                  className="font-mono"
                  disabled={mutation.isPending}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
        <p className="text-xs text-muted-foreground">
          Use / to check the site root, or a path such as /health. The monitor
          uses this environment’s generated domain. Changes apply to the next
          check; no redeploy is needed.
        </p>
        <p className="text-xs text-muted-foreground">
          Automatically created monitors follow deployment settings again after
          the next successful deployment. For Docker Compose, configure the
          first public route’s Health path to keep this choice across
          deployments.
        </p>
        {form.formState.errors.root && (
          <p role="alert" className="text-sm text-destructive">
            {form.formState.errors.root.message}
          </p>
        )}
        <Button
          type="submit"
          size="sm"
          disabled={mutation.isPending || !form.formState.isDirty}
        >
          {mutation.isPending ? 'Saving…' : 'Save monitor path'}
        </Button>
      </form>
    </Form>
  )
}

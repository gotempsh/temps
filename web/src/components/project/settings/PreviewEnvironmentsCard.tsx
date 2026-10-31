// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { updateProjectSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useForm, useFormState, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z
  .object({
    enablePreviewEnvironments: z.boolean(),
    previewEnvsOnDemand: z.boolean(),
    previewEnvsIdleTimeoutSeconds: z.string(),
    previewEnvsWakeTimeoutSeconds: z.string(),
  })
  .superRefine((values, ctx) => {
    if (!values.previewEnvsOnDemand) return
    const idle = parseInt(values.previewEnvsIdleTimeoutSeconds, 10)
    if (Number.isNaN(idle) || idle < 60 || idle > 86400) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ['previewEnvsIdleTimeoutSeconds'],
        message: 'Must be between 60 and 86400 seconds',
      })
    }
    const wake = parseInt(values.previewEnvsWakeTimeoutSeconds, 10)
    if (Number.isNaN(wake) || wake < 5 || wake > 120) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ['previewEnvsWakeTimeoutSeconds'],
        message: 'Must be between 5 and 120 seconds',
      })
    }
  })

type FormValues = z.infer<typeof schema>

/** Per-branch preview environments and their on-demand sleep behaviour. */
export function PreviewEnvironmentsCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: { errorTitle: 'Failed to update preview environment settings' },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      enablePreviewEnvironments: project?.enable_preview_environments ?? false,
      previewEnvsOnDemand: project?.preview_envs_on_demand ?? false,
      previewEnvsIdleTimeoutSeconds: (
        project?.preview_envs_idle_timeout_seconds ?? 300
      ).toString(),
      previewEnvsWakeTimeoutSeconds: (
        project?.preview_envs_wake_timeout_seconds ?? 30
      ).toString(),
    },
  })

  const previewEnabled = useWatch({
    control: form.control,
    name: 'enablePreviewEnvironments',
  })
  const onDemandEnabled = useWatch({
    control: form.control,
    name: 'previewEnvsOnDemand',
  })

  const { isDirty } = useFormState({ control: form.control })

  // `toast.promise` returns a toast handle, not the request: await the
  // mutation itself so the refetch sees the saved settings.
  const handleSave = async (values: FormValues) => {
    if (!project?.id) return
    try {
      await updateProjectSettings.mutateAsync({
        path: { project_id: project.id },
        body: {
          enable_preview_environments: values.enablePreviewEnvironments,
          preview_envs_on_demand: values.previewEnvsOnDemand,
          preview_envs_idle_timeout_seconds: parseInt(
            values.previewEnvsIdleTimeoutSeconds,
            10
          ),
          preview_envs_wake_timeout_seconds: parseInt(
            values.previewEnvsWakeTimeoutSeconds,
            10
          ),
        },
      })
    } catch {
      // The mutation's errorTitle already explains the failure.
      return
    }
    form.reset(values)
    toast.success('Preview environments saved')
    refetch()
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card id="setting-preview-environments" className="scroll-mt-4">
          <CardHeader>
            <CardTitle>Preview environments</CardTitle>
            <CardDescription>
              A temporary environment for each branch or pull request that
              matches no other environment.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="divide-y rounded-md border">
              <FormField
                control={form.control}
                name="enablePreviewEnvironments"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-start justify-between gap-4 space-y-0 px-4 py-3">
                    <div className="space-y-0.5">
                      <FormLabel>Create previews</FormLabel>
                      <FormDescription>
                        Deploy feature branches and pull requests to their own
                        environment.
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="previewEnvsOnDemand"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-start justify-between gap-4 space-y-0 px-4 py-3">
                    <div className="space-y-0.5">
                      <FormLabel>Sleep when idle</FormLabel>
                      <FormDescription>
                        Stop a preview&apos;s containers after the idle timeout
                        and start them again on the next request. Applies to
                        previews created after it is turned on.
                      </FormDescription>
                    </div>
                    <FormControl>
                      <Switch
                        checked={field.value}
                        onCheckedChange={field.onChange}
                        disabled={!previewEnabled}
                      />
                    </FormControl>
                  </FormItem>
                )}
              />
            </div>

            {onDemandEnabled && previewEnabled && (
              <div className="grid gap-4 sm:grid-cols-2">
                <FormField
                  control={form.control}
                  name="previewEnvsIdleTimeoutSeconds"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Idle timeout (seconds)</FormLabel>
                      <FormControl>
                        <Input type="number" min={60} max={86400} {...field} />
                      </FormControl>
                      <FormDescription>
                        Inactivity before containers stop. 60 to 86400; default
                        300.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="previewEnvsWakeTimeoutSeconds"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Wake timeout (seconds)</FormLabel>
                      <FormControl>
                        <Input type="number" min={5} max={120} {...field} />
                      </FormControl>
                      <FormDescription>
                        Longest wait for containers to start on wake. 5 to 120;
                        default 30.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            )}
          </CardContent>
          <CardFooter>
            <p>
              {!previewEnabled &&
                'With previews off, pushes to other branches do not deploy.'}
            </p>
            <Button
              type="submit"
              size="sm"
              disabled={!isDirty || updateProjectSettings.isPending}
            >
              {updateProjectSettings.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Save
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}

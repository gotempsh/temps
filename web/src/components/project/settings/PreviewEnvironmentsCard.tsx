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
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
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

  const previewEnabled = form.watch('enablePreviewEnvironments')
  const onDemandEnabled = form.watch('previewEnvsOnDemand')

  const handleSave = async (values: FormValues) => {
    if (!project?.id) return
    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! },
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
      }),
      {
        loading: 'Updating preview environment settings...',
        success: 'Preview environment settings updated successfully',
        error: 'Failed to update preview environment settings',
      }
    )
    refetch()
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card className="bg-background text-foreground">
          <CardHeader>
            <CardTitle>Preview Environments</CardTitle>
            <CardDescription>
              Automatically create preview environments for each branch. When
              enabled, deployments to branches that don&apos;t match any
              existing environment will create temporary preview environments.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <FormField
              control={form.control}
              name="enablePreviewEnvironments"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Enable Preview Environments
                    </FormLabel>
                    <FormDescription>
                      Automatically create environments for feature branches,
                      pull requests, and other non-production branches
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
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      On-Demand Preview Environments
                    </FormLabel>
                    <FormDescription>
                      Save resources by sleeping preview environments when idle.
                      Containers stop after the idle timeout and start again on
                      the next request. Applies only to previews created after
                      this is enabled.
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
                        Seconds of inactivity before containers are stopped. Min
                        60, max 86400 (24h). Default 300 (5 min).
                      </FormDescription>
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
                        Max time to wait for containers to start on wake. Min 5,
                        max 120. Default 30.
                      </FormDescription>
                    </FormItem>
                  )}
                />
              </div>
            )}
          </CardContent>
          <CardFooter>
            <Button type="submit" disabled={updateProjectSettings.isPending}>
              Save Settings
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}

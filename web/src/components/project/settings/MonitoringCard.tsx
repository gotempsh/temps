import { ProjectResponse } from '@/api/client'
import { updateProjectDeploymentConfigMutation } from '@/api/client/@tanstack/react-query.gen'
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
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  performanceMetricsEnabled: z.boolean(),
  sessionRecordingEnabled: z.boolean(),
})

type FormValues = z.infer<typeof schema>

/**
 * Observability toggles that ride on the deployment config.
 *
 * These live here rather than under Build & Deploy because they describe what
 * a running deployment reports, not how it is built or shipped. They share the
 * deployment-config endpoint with [DeployDefaultsCard], but each card sends
 * only its own fields and the service applies `if let Some(..)` per field, so
 * neither resets the other.
 */
export function MonitoringCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: { errorTitle: 'Failed to update monitoring settings' },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      performanceMetricsEnabled:
        project?.deployment_config?.performanceMetricsEnabled ?? false,
      sessionRecordingEnabled:
        project?.deployment_config?.sessionRecordingEnabled ?? false,
    },
  })

  const handleSave = async (values: FormValues) => {
    if (!project?.id) return
    await toast.promise(
      updateDeploymentConfig.mutateAsync({
        path: { project_id: project.id! },
        body: {
          performanceMetricsEnabled: values.performanceMetricsEnabled,
          sessionRecordingEnabled: values.sessionRecordingEnabled,
        },
      }),
      {
        loading: 'Updating monitoring settings...',
        success: 'Monitoring settings updated successfully',
        error: 'Failed to update monitoring settings',
      }
    )
    refetch()
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card className="bg-background text-foreground">
          <CardHeader>
            <CardTitle>Monitoring</CardTitle>
            <CardDescription>
              What this project&apos;s deployments collect about themselves at
              runtime.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <FormField
              control={form.control}
              name="performanceMetricsEnabled"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Performance Metrics
                    </FormLabel>
                    <FormDescription>
                      Collect and display performance metrics for your
                      deployments
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
              name="sessionRecordingEnabled"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                  <div className="space-y-0.5">
                    <FormLabel className="text-base">
                      Session Recording
                    </FormLabel>
                    <FormDescription>
                      Record user sessions for debugging and analytics
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
          </CardContent>
          <CardFooter>
            <Button type="submit" disabled={updateDeploymentConfig.isPending}>
              Save Monitoring
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}

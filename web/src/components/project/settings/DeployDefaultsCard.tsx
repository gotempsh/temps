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
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const schema = z.object({
  cpuRequest: z.string().optional(),
  cpuLimit: z.string().optional(),
  memoryRequest: z.string().optional(),
  memoryLimit: z.string().optional(),
  replicas: z.string().optional(),
  port: z.string().optional(),
  automaticDeploy: z.boolean(),
})

type FormValues = z.infer<typeof schema>

/** Blank input means "no override" — send null rather than 0/NaN. */
function optionalInt(value: string | undefined): number | null {
  return value && value.trim() !== '' ? parseInt(value, 10) : null
}

/** CPU is entered in cores and stored in millionths of a core. */
function optionalCores(value: string | undefined): number | null {
  return value && value.trim() !== ''
    ? Math.round(parseFloat(value) * 1_000_000)
    : null
}

/**
 * Resource defaults and deploy automation for every environment.
 *
 * Submits only the fields it owns. `update_project_deployment_config` applies
 * `if let Some(..)` per field, so the monitoring toggles this card does not
 * render are left untouched rather than reset — which is what makes it safe to
 * split the old single "Default Deployment Configuration" form across tabs.
 */
export function DeployDefaultsCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: { errorTitle: 'Failed to update deployment configuration' },
  })

  const form = useForm<FormValues>({
    resolver: zodResolver(schema),
    defaultValues: {
      cpuRequest:
        project?.deployment_config?.cpuRequest != null
          ? (project.deployment_config.cpuRequest / 1_000_000).toString()
          : '',
      cpuLimit:
        project?.deployment_config?.cpuLimit != null
          ? (project.deployment_config.cpuLimit / 1_000_000).toString()
          : '',
      memoryRequest:
        project?.deployment_config?.memoryRequest?.toString() ?? '',
      memoryLimit: project?.deployment_config?.memoryLimit?.toString() ?? '',
      replicas: project?.deployment_config?.replicas?.toString() ?? '',
      port: project?.deployment_config?.exposedPort?.toString() ?? '',
      automaticDeploy: project?.deployment_config?.automaticDeploy ?? false,
    },
  })

  const handleSave = async (values: FormValues) => {
    if (!project?.id) return
    await toast.promise(
      updateDeploymentConfig.mutateAsync({
        path: { project_id: project.id! },
        body: {
          cpuRequest: optionalCores(values.cpuRequest),
          cpuLimit: optionalCores(values.cpuLimit),
          memoryRequest: optionalInt(values.memoryRequest),
          memoryLimit: optionalInt(values.memoryLimit),
          replicas: optionalInt(values.replicas),
          exposedPort: optionalInt(values.port),
          automaticDeploy: values.automaticDeploy,
        },
      }),
      {
        loading: 'Updating deployment configuration...',
        success: 'Deployment configuration updated successfully',
        error: 'Failed to update deployment configuration',
      }
    )
    refetch()
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(handleSave)}>
        <Card className="bg-background text-foreground">
          <CardHeader>
            <CardTitle>Default Deployment Configuration</CardTitle>
            <CardDescription>
              Configure default resource limits and deployment settings for all
              environments. These can be overridden per environment.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="space-y-4">
              <h3 className="text-sm font-medium">Resource Limits</h3>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                <FormField
                  control={form.control}
                  name="cpuRequest"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>CPU Request (cores)</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          step="any"
                          min="0.01"
                          placeholder="e.g., 0.1"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Minimum CPU cores (e.g., 0.25, 0.5, 1, 2)
                      </FormDescription>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="cpuLimit"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>CPU Limit (cores)</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          step="any"
                          min="0.01"
                          placeholder="e.g., 1"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Maximum CPU cores (e.g., 0.5, 1, 2)
                      </FormDescription>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="memoryRequest"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Memory Request (MB)</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          min="1"
                          placeholder="e.g., 128"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Minimum memory allocation
                      </FormDescription>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="memoryLimit"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Memory Limit (MB)</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          min="0"
                          placeholder="e.g., 256"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Maximum memory allocation. Leave empty to use the
                        default, or set <code>0</code> to run uncapped.
                      </FormDescription>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="replicas"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Default Replicas</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          min="1"
                          placeholder="e.g., 1"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Default number of container instances
                      </FormDescription>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="port"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Default Port</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          type="number"
                          min="1"
                          max="65535"
                          placeholder="e.g., 3000"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Default port your application listens on
                      </FormDescription>
                    </FormItem>
                  )}
                />
              </div>
            </div>

            <div className="space-y-4">
              <h3 className="text-sm font-medium">Automation</h3>
              <FormField
                control={form.control}
                name="automaticDeploy"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-center justify-between rounded-lg border p-4">
                    <div className="space-y-0.5">
                      <FormLabel className="text-base">
                        Automatic Deployments
                      </FormLabel>
                      <FormDescription>
                        Automatically deploy when changes are pushed to the main
                        branch
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
            </div>
          </CardContent>
          <CardFooter>
            <Button type="submit" disabled={updateDeploymentConfig.isPending}>
              Save Configuration
            </Button>
          </CardFooter>
        </Card>
      </form>
    </Form>
  )
}

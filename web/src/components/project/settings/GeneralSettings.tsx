import { ProjectResponse } from '@/api/client'
import {
  deleteProjectMutation,
  updateProjectDeploymentConfigMutation,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
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
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

interface GeneralSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

const projectSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  dockerfilePath: z.string().optional(),
})

type ProjectFormValues = z.infer<typeof projectSchema>

const deploymentConfigSchema = z.object({
  cpuRequest: z.string().optional(),
  cpuLimit: z.string().optional(),
  memoryRequest: z.string().optional(),
  memoryLimit: z.string().optional(),
  replicas: z.string().optional(),
  automaticDeploy: z.boolean(),
  performanceMetricsEnabled: z.boolean(),
  sessionRecordingEnabled: z.boolean(),
})

type DeploymentConfigFormValues = z.infer<typeof deploymentConfigSchema>

export function GeneralSettings({ project, refetch }: GeneralSettingsProps) {
  const navigate = useNavigate()
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update project settings',
    },
  })

  const updateDeploymentConfig = useMutation({
    ...updateProjectDeploymentConfigMutation(),
    meta: {
      errorTitle: 'Failed to update deployment configuration',
    },
  })

  const projectForm = useForm<ProjectFormValues>({
    resolver: zodResolver(projectSchema),
    defaultValues: {
      name: project?.slug || '',
      dockerfilePath: 'Dockerfile',
    },
  })

  const deploymentForm = useForm<DeploymentConfigFormValues>({
    resolver: zodResolver(deploymentConfigSchema),
    defaultValues: {
      cpuRequest: project?.cpu_request?.toString() ?? '',
      cpuLimit: project?.cpu_limit?.toString() ?? '',
      memoryRequest: project?.memory_request?.toString() ?? '',
      memoryLimit: project?.memory_limit?.toString() ?? '',
      replicas: '',
      automaticDeploy: project?.automatic_deploy ?? false,
      performanceMetricsEnabled: project?.performance_metrics_enabled ?? false,
      sessionRecordingEnabled: false,
    },
  })

  const handleSaveProject = async (values: ProjectFormValues) => {
    if (!project?.id) return

    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id!.toString() },
        body: {
          slug: values.name,
        },
      }),
      {
        loading: 'Updating project...',
        success: 'Project updated successfully',
        error: 'Failed to update project',
      }
    )
    refetch()
    navigate(`/projects/${values.name}/settings/general`)
  }

  const handleSaveDeploymentConfig = async (
    values: DeploymentConfigFormValues
  ) => {
    if (!project?.id) return

    await toast.promise(
      updateDeploymentConfig.mutateAsync({
        path: { project_id: project.id!.toString() },
        body: {
          cpu_request:
            values.cpuRequest && values.cpuRequest.trim() !== ''
              ? parseInt(values.cpuRequest)
              : null,
          cpu_limit:
            values.cpuLimit && values.cpuLimit.trim() !== ''
              ? parseInt(values.cpuLimit)
              : null,
          memory_request:
            values.memoryRequest && values.memoryRequest.trim() !== ''
              ? parseInt(values.memoryRequest)
              : null,
          memory_limit:
            values.memoryLimit && values.memoryLimit.trim() !== ''
              ? parseInt(values.memoryLimit)
              : null,
          replicas:
            values.replicas && values.replicas.trim() !== ''
              ? parseInt(values.replicas)
              : null,
          automatic_deploy: values.automaticDeploy,
          performance_metrics_enabled: values.performanceMetricsEnabled,
          session_recording_enabled: values.sessionRecordingEnabled,
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

  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const deleteProjectMutationM = useMutation({
    ...deleteProjectMutation(),
    meta: {
      errorTitle: 'Failed to delete project',
    },
  })

  const handleDeleteProject = async () => {
    setIsDeleteDialogOpen(false)
    try {
      await toast.promise(
        deleteProjectMutationM.mutateAsync({
          path: { id: project?.id! as number },
        }),
        {
          loading: 'Deleting project...',
          success: () => {
            navigate(`/projects`, {})
            return 'Project deleted'
          },
          error: 'Failed to delete project',
        }
      )
    } catch (error) {
      console.error('Error deleting project:', error)
    }
  }

  return (
    <div className="space-y-6">
      {/* Project Settings Card */}
      <Form {...projectForm}>
        <form onSubmit={projectForm.handleSubmit(handleSaveProject)}>
          <Card className="bg-background text-foreground">
            <CardHeader>
              <CardTitle>Project Settings</CardTitle>
              <CardDescription>
                Used to identify your Project on the Dashboard, CLI, and in the
                URL of your Deployments.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <FormField
                control={projectForm.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Project Slug</FormLabel>
                    <FormControl>
                      <Input {...field} className="max-w-[400px]" />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      This will be used in your project&apos;s URL
                    </FormDescription>
                  </FormItem>
                )}
              />

              {project?.preset?.toLowerCase().includes('docker') && (
                <FormField
                  control={projectForm.control}
                  name="dockerfilePath"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Dockerfile Path</FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          placeholder="Dockerfile"
                          className="max-w-[400px]"
                        />
                      </FormControl>
                      <FormDescription className="text-muted-foreground">
                        Path to your Dockerfile relative to the root directory
                      </FormDescription>
                    </FormItem>
                  )}
                />
              )}
            </CardContent>
            <CardFooter>
              <Button type="submit" disabled={updateProjectSettings.isPending}>
                Save
              </Button>
            </CardFooter>
          </Card>
        </form>
      </Form>

      {/* Deployment Configuration Card */}
      <Form {...deploymentForm}>
        <form
          onSubmit={deploymentForm.handleSubmit(handleSaveDeploymentConfig)}
        >
          <Card className="bg-background text-foreground">
            <CardHeader>
              <CardTitle>Default Deployment Configuration</CardTitle>
              <CardDescription>
                Configure default resource limits and deployment settings for all
                environments. These can be overridden per environment.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {/* Resource Limits */}
              <div className="space-y-4">
                <h3 className="text-sm font-medium">Resource Limits</h3>
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  <FormField
                    control={deploymentForm.control}
                    name="cpuRequest"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>CPU Request (millicores)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            type="number"
                            min="1"
                            placeholder="e.g., 100"
                          />
                        </FormControl>
                        <FormDescription className="text-muted-foreground">
                          Minimum CPU resources (1000m = 1 CPU core)
                        </FormDescription>
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={deploymentForm.control}
                    name="cpuLimit"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>CPU Limit (millicores)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            type="number"
                            min="1"
                            placeholder="e.g., 200"
                          />
                        </FormControl>
                        <FormDescription className="text-muted-foreground">
                          Maximum CPU resources (1000m = 1 CPU core)
                        </FormDescription>
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={deploymentForm.control}
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
                    control={deploymentForm.control}
                    name="memoryLimit"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Memory Limit (MB)</FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            type="number"
                            min="1"
                            placeholder="e.g., 256"
                          />
                        </FormControl>
                        <FormDescription className="text-muted-foreground">
                          Maximum memory allocation
                        </FormDescription>
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={deploymentForm.control}
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
                </div>
              </div>

              {/* Automation Settings */}
              <div className="space-y-4">
                <h3 className="text-sm font-medium">Automation</h3>
                <FormField
                  control={deploymentForm.control}
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

              {/* Monitoring Settings */}
              <div className="space-y-4">
                <h3 className="text-sm font-medium">Monitoring</h3>
                <div className="space-y-4">
                  <FormField
                    control={deploymentForm.control}
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
                    control={deploymentForm.control}
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
                </div>
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

      {/* Danger Zone */}
      <div className="border-t pt-6">
        <h3 className="text-lg font-medium text-destructive">Danger Zone</h3>
        <p className="text-sm text-muted-foreground mt-1 mb-4">
          Permanently delete this project and all of its contents from the
          platform. This action is not reversible, so please continue with
          caution.
        </p>
        <AlertDialog
          open={isDeleteDialogOpen}
          onOpenChange={setIsDeleteDialogOpen}
        >
          <AlertDialogTrigger asChild>
            <Button variant="destructive">Delete project</Button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Are you absolutely sure?</AlertDialogTitle>
              <AlertDialogDescription>
                This action cannot be undone. This will permanently delete your
                project &quot;{project?.name}&quot; and remove all associated
                data from our servers.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={handleDeleteProject}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Delete
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  )
}

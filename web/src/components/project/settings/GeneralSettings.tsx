import { ProjectResponse } from '@/api/client'
import {
  deleteProjectMutation,
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
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useState } from 'react'
import { SubmitHandler, useForm } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'

interface GeneralSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

const projectSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  port: z.coerce.number().min(1).max(65535).optional(),
  dockerfilePath: z.string().optional(),
})

type ProjectFormValues = z.infer<typeof projectSchema>

export function GeneralSettings({ project, refetch }: GeneralSettingsProps) {
  const navigate = useNavigate()
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update project settings',
    },
  })

  const projectForm = useForm<ProjectFormValues>({
    resolver: zodResolver(projectSchema),
    defaultValues: {
      name: project?.slug || '',
      port: project?.exposed_port ? Number(project.exposed_port) : undefined,
      dockerfilePath: 'Dockerfile',
    },
  })

  const handleSaveProject = async (values: ProjectFormValues) => {
    if (!project?.id) return

    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! as number },
        body: {
          slug: values.name,
          exposed_port: values.port,
        },
      }),
      {
        loading: 'Updating project...',
        success: 'Project updated successfully',
        error: 'Failed to update project',
      }
    )
    await refetch()
    navigate(`/projects/${values.name}/settings/general`)
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
          success: (_) => {
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
      <Form {...projectForm}>
        <form
          onSubmit={projectForm.handleSubmit(
            handleSaveProject as SubmitHandler<ProjectFormValues>
          )}
        >
          <Card className="bg-background text-foreground">
            <CardHeader>
              <CardTitle>Project slug</CardTitle>
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
                    <FormControl>
                      <Input {...field} className="max-w-[400px]" />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      This will be used in your project&apos;s URL
                    </FormDescription>
                  </FormItem>
                )}
              />

              <FormField
                control={projectForm.control}
                name="port"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Exposed Port (Override)</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="number"
                        min="1"
                        max="65535"
                        placeholder="Auto-detected from image"
                        className="max-w-[400px]"
                      />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      Optional: Override the port when your image has no EXPOSE
                      directive. Priority: Image EXPOSE → Environment override →
                      This value → Default (3000)
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
        {/* <Button variant="destructive">Delete Project</Button> */}
      </div>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { MonitoringCard } from './MonitoringCard'
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
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

interface GeneralSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

const projectSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Project name is required')
    .max(100, 'Project name must be 100 characters or fewer'),
  slug: z
    .string()
    .trim()
    .min(1, 'Project slug is required')
    .max(63, 'Project slug must be 63 characters or fewer')
    .regex(
      /^[a-z0-9]+(?:-[a-z0-9]+)*$/,
      'Use lowercase letters, numbers and single hyphens (no leading or trailing hyphen)'
    ),
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
      name: project?.name || '',
      slug: project?.slug || '',
    },
  })

  // `defaultValues` are only read on mount, but this component stays mounted
  // when the route switches between two projects' settings pages. Without this
  // reset the form would still hold the previous project's identity, and Save
  // would rename the newly-selected project to the old one's name and slug.
  //
  // Keyed on the project *identity*, not its values: a plain refetch of the
  // same project must not overwrite whatever the user is currently typing.
  const syncedProjectId = useRef<number | undefined>(undefined)
  useEffect(() => {
    if (project?.id === undefined || syncedProjectId.current === project.id) {
      return
    }
    syncedProjectId.current = project.id
    projectForm.reset({
      name: project.name || '',
      slug: project.slug || '',
    })
  }, [project?.id, project?.name, project?.slug, projectForm])

  const handleSaveProject = async (values: ProjectFormValues) => {
    if (!project?.id) return

    const request = updateProjectSettings.mutateAsync({
      path: { project_id: project.id! },
      body: {
        name: values.name,
        slug: values.slug,
      },
    })
    toast.promise(request, {
      loading: 'Updating project...',
      success: 'Project updated successfully',
      error: 'Failed to update project',
    })
    // The toast surfaces the failure; bail out here so a rejected save never
    // falls through to refetch/navigate, and never escapes as an unhandled
    // rejection.
    let updated
    try {
      updated = await request
    } catch {
      return
    }
    refetch()
    // Navigate to the slug the server persisted, not the one submitted: the
    // server normalizes it, so routing on the raw input can land on a URL that
    // does not exist.
    navigate(`/projects/${updated?.slug ?? values.slug}/settings/general`)
  }

  const handleToggleCrossProjectTraceSharing = async (enabled: boolean) => {
    if (!project?.id) return

    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! },
        body: {
          cross_project_trace_sharing: enabled,
        },
      }),
      {
        loading: 'Updating cross-project trace sharing...',
        success: 'Cross-project trace sharing updated',
        error: 'Failed to update cross-project trace sharing',
      }
    )
    refetch()
  }

  const handleToggleErrorSourceContext = async (enabled: boolean) => {
    if (!project?.id) return

    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! },
        body: {
          error_source_context_enabled: enabled,
        },
      }),
      {
        loading: 'Updating source context setting...',
        success: 'Error tracking source context updated',
        error: 'Failed to update source context setting',
      }
    )
    refetch()
  }

  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [deleteConfirmName, setDeleteConfirmName] = useState('')
  const deleteProjectMutationM = useMutation({
    ...deleteProjectMutation(),
    meta: {
      errorTitle: 'Failed to delete project',
    },
  })

  const handleDeleteProject = async () => {
    if (deleteConfirmName.trim() !== project?.name) return
    setIsDeleteDialogOpen(false)
    try {
      await toast.promise(
        deleteProjectMutationM.mutateAsync({
          path: { id: project.id! },
        }),
        {
          loading: 'Deleting project...',
          success: () => {
            navigate('/projects')
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
                    <FormLabel>Project Name</FormLabel>
                    <FormControl>
                      <Input {...field} className="max-w-[400px]" />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      The display name shown on the dashboard, in alerts, and in
                      notifications. Also used as the OpenTelemetry service name
                      for future deployments, so renaming starts a new series in
                      traces and metrics.
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={projectForm.control}
                name="slug"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Project Slug</FormLabel>
                    <FormControl>
                      <Input {...field} className="max-w-[400px]" />
                    </FormControl>
                    <FormDescription className="text-muted-foreground">
                      This will be used in your project&apos;s URL
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />
            </CardContent>
            <CardFooter>
              <Button type="submit" disabled={updateProjectSettings.isPending}>
                Save
              </Button>
            </CardFooter>
          </Card>
        </form>
      </Form>

      {/* Monitoring — what deployments report about themselves */}
      <MonitoringCard project={project} refetch={refetch} />

      {/* Cross-Project Trace Sharing Card */}
      <Card className="bg-background text-foreground">
        <CardHeader>
          <CardTitle>Cross-Project Trace Sharing</CardTitle>
          <CardDescription>
            Control whether this project's spans can appear in other projects'
            unified cross-project traces.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex flex-row items-center justify-between rounded-lg border p-4">
            <div className="space-y-0.5 pr-4">
              <Label className="text-base">Cross-project trace sharing</Label>
              <p className="text-sm text-muted-foreground">
                When on, this project's spans appear in other projects' unified
                cross-project traces. Turn off to keep this project's spans
                private to itself.
              </p>
            </div>
            <Switch
              checked={project?.cross_project_trace_sharing ?? true}
              onCheckedChange={handleToggleCrossProjectTraceSharing}
              disabled={updateProjectSettings.isPending}
            />
          </div>
        </CardContent>
      </Card>

      {/* Error Tracking Source Context Card */}
      <Card className="bg-background text-foreground">
        <CardHeader>
          <CardTitle>Error Tracking Source Context</CardTitle>
          <CardDescription>
            Show the actual source code around each stack frame in error
            reports. JavaScript source maps always resolve; enable this to also
            store uploaded source files and render code for native stack traces
            (Go, Rust, Python, and more).
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex flex-row items-center justify-between rounded-lg border p-4">
            <div className="space-y-0.5 pr-4">
              <Label className="text-base">Source code in stack traces</Label>
              <p className="text-sm text-muted-foreground">
                Off by default. When enabled, upload your application source per
                release (via the CLI or API, keyed by the deployed commit/tag)
                and Temps shows the code around each frame. Source files are
                only accepted and stored while this is on.
              </p>
            </div>
            <Switch
              checked={project?.error_source_context_enabled ?? false}
              onCheckedChange={handleToggleErrorSourceContext}
              disabled={updateProjectSettings.isPending}
            />
          </div>
        </CardContent>
      </Card>

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
          onOpenChange={(open) => {
            setIsDeleteDialogOpen(open)
            if (!open) setDeleteConfirmName('')
          }}
        >
          <AlertDialogTrigger asChild>
            <Button variant="destructive">Delete project</Button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Are you absolutely sure?</AlertDialogTitle>
              <AlertDialogDescription>
                This action cannot be undone. This will permanently delete this
                project and remove all associated data from our servers.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <div className="space-y-2">
              <Label htmlFor="confirm-delete-project-name">
                Type{' '}
                <span className="font-mono font-semibold text-foreground">
                  {project?.name}
                </span>{' '}
                to confirm
              </Label>
              <Input
                id="confirm-delete-project-name"
                value={deleteConfirmName}
                onChange={(e) => setDeleteConfirmName(e.target.value)}
                placeholder={project?.name}
                autoComplete="off"
              />
            </div>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={handleDeleteProject}
                disabled={
                  deleteProjectMutationM.isPending ||
                  deleteConfirmName.trim() !== project?.name
                }
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                {deleteProjectMutationM.isPending ? 'Deleting...' : 'Delete'}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  )
}

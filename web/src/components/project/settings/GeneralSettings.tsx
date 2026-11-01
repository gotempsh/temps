import { SaveRow, SettingsGroup } from '@temps-sdk/ds'
import { SettingToggleRow } from '@/components/settings/SettingToggleRow'
// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import {
  deleteProjectMutation,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CloudTelemetryBackfillCard } from './CloudTelemetryBackfillCard'
import { MonitoringToggles } from './MonitoringCard'
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
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
    .min(1, 'Service name is required')
    .max(100, 'Service name must be 100 characters or fewer'),
  slug: z
    .string()
    .trim()
    .min(1, 'Service slug is required')
    .max(63, 'Service slug must be 63 characters or fewer')
    .regex(
      /^[a-z0-9]+(?:-[a-z0-9]+)*$/,
      'Use lowercase letters, numbers and single hyphens (no leading or trailing hyphen)'
    ),
})

type ProjectFormValues = z.infer<typeof projectSchema>

export function GeneralSettings({ project, refetch }: GeneralSettingsProps) {
  const navigate = useNavigate()
  // Renaming a project onto — or off — a slug this host grants the Docker
  // socket to is a sensitive action (ADR 045), so the save can come back 428
  // asking the admin to re-verify rather than failing. Every other save here
  // is unaffected and never reaches the dialog.
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update service settings',
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
    // Hand-rolled rather than `toast.promise`, because one outcome is neither
    // success nor failure: a 428 means "prove it's you and this will go
    // through". Attaching a fixed error toast up front would flash a red
    // "Failed to update project" behind the verification dialog for a save
    // that is about to succeed.
    const toastId = toast.loading('Updating service...')
    let updated
    try {
      updated = await request
    } catch (error) {
      toast.dismiss(toastId)
      // Opens the step-up dialog and re-runs this save once verified. The
      // global mutation handler already suppresses its own toast for
      // STEP_UP_REQUIRED, so nothing else fires in the meantime.
      if (
        handleSensitiveActionError(error, () => void handleSaveProject(values))
      ) {
        return
      }
      // Any other failure is toasted by the app-wide mutation handler.
      return
    }
    toast.success('Service updated successfully', { id: toastId })
    // The saved identity is the new baseline, so Save disables again.
    projectForm.reset({
      name: updated?.name ?? values.name,
      slug: updated?.slug ?? values.slug,
    })
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
        loading: 'Updating cross-service trace sharing...',
        success: 'Cross-service trace sharing updated',
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
      }
    )
    refetch()
  }

  const handleToggleServesOwnTempsApi = async (enabled: boolean) => {
    if (!project?.id) return

    await toast.promise(
      updateProjectSettings.mutateAsync({
        path: { project_id: project.id! },
        body: {
          serves_own_temps_api: enabled,
        },
      }),
      {
        loading: 'Updating Temps API routing...',
        success: enabled
          ? 'Temps API paths now reach this service'
          : 'Temps API paths now reach this Temps instance',
      }
    )
    refetch()
  }

  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const deleteProjectMutationM = useMutation({
    ...deleteProjectMutation(),
    meta: {
      errorTitle: 'Failed to delete service',
    },
  })

  const handleDeleteProject = async () => {
    setIsDeleteDialogOpen(false)
    try {
      await toast.promise(
        deleteProjectMutationM.mutateAsync({
          path: { id: project.id! },
        }),
        {
          loading: 'Deleting service...',
          success: () => {
            navigate('/projects')
            return 'Service deleted'
          },
        }
      )
    } catch (error) {
      console.error('Error deleting project:', error)
    }
  }

  return (
    <div className="divide-y [&>section]:py-8 [&>section:first-of-type]:pt-0">
      {verificationDialog}
      <SettingsGroup
        id="setting-identity"
        title="Identity"
        description="How this service is named in the console, its URL, alerts and traces."
      >
        <Form {...projectForm}>
          <form
            onSubmit={projectForm.handleSubmit(handleSaveProject)}
            className="space-y-5"
          >
            <FormField
              control={projectForm.control}
              name="name"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Service name</FormLabel>
                  <FormControl>
                    <Input {...field} className="max-w-md" />
                  </FormControl>
                  <FormDescription>
                    Also the OpenTelemetry service name for future deployments,
                    so renaming starts a new series in traces and metrics.
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
                  <FormLabel>Service slug</FormLabel>
                  <FormControl>
                    <Input {...field} className="max-w-md" />
                  </FormControl>
                  <FormDescription>
                    Used in this service&apos;s URL.
                  </FormDescription>
                  <FormMessage />
                </FormItem>
              )}
            />
            <SaveRow
              dirty={projectForm.formState.isDirty}
              saving={updateProjectSettings.isPending}
            />
          </form>
        </Form>
      </SettingsGroup>

      <SettingsGroup
        title="Telemetry"
        description="What this service reports, and who else can see it."
      >
        <div className="divide-y">
          {project.source_type !== 'external' && (
            <MonitoringToggles project={project} refetch={refetch} />
          )}
          <SettingToggleRow
            id="cross-project-trace-sharing"
            label="Cross-service trace sharing"
            description="Show this service's spans in other services' unified traces. Off keeps them private to this service."
            checked={project?.cross_project_trace_sharing ?? true}
            onCheckedChange={handleToggleCrossProjectTraceSharing}
            disabled={updateProjectSettings.isPending}
          />
          <SettingToggleRow
            id="error-source-context"
            label="Source code in stack traces"
            description="Upload source per release (CLI or API) and errors show the code around each frame. Source is only accepted while this is on."
            checked={project?.error_source_context_enabled ?? false}
            onCheckedChange={handleToggleErrorSourceContext}
            disabled={updateProjectSettings.isPending}
          />
          <SettingToggleRow
            id="serves-own-temps-api"
            label="App serves its own Temps API"
            description={
              <>
                Send <code>/api/_temps</code> and <code>/api/otel</code> on this
                service&apos;s domains to the app instead of Temps. Only for an
                app that is itself a Temps console; Temps&apos; analytics and
                ingest then stop receiving its traffic.
              </>
            }
            checked={project?.serves_own_temps_api ?? false}
            onCheckedChange={handleToggleServesOwnTempsApi}
            disabled={updateProjectSettings.isPending}
          />
          {/* ADR-040: the backfill is a deliberate CLI action, so its status
              here is the only place it is discoverable from the Console. */}
          <div className="space-y-3 py-4 last:pb-0">
            <p className="text-sm font-medium">Cloud telemetry backfill</p>
            <CloudTelemetryBackfillCard project={project} bare />
          </div>
        </div>
      </SettingsGroup>

      <SettingsGroup
        id="setting-danger-zone"
        title="Danger zone"
        description="Permanently delete this service and everything in it. This cannot be undone."
      >
        <div>
          <Button
            variant="destructive"
            onClick={() => setIsDeleteDialogOpen(true)}
          >
            Delete service
          </Button>
        </div>
        <ConfirmDeleteDialog
          open={isDeleteDialogOpen}
          onOpenChange={setIsDeleteDialogOpen}
          resourceType="service"
          resourceName={project?.name ?? ''}
          consequences="Its deployments, environments, domains, environment variables and all associated data are permanently removed."
          requireTypedName
          onConfirm={() => void handleDeleteProject()}
        />
      </SettingsGroup>
    </div>
  )
}

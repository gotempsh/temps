// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  deleteEnvironmentMutation,
  getEnvironmentOptions,
  updateEnvironmentSubdomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { SettingsSection } from '@/components/ui/settings-section'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useSettings } from '@/hooks/useSettings'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Globe, HardDrive, Loader2, RefreshCw, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { useParams } from 'react-router'
import { toast } from 'sonner'
import { EnvironmentConfigurationCard } from './EnvironmentConfigurationCard'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'

interface EnvironmentDetailProps {
  project: ProjectResponse
  environmentId?: number // Optional: if not provided, will use useParams
  initialEnvironment?: EnvironmentResponse // Optional: initial environment data to use as default
  onDelete?: () => void // Optional: callback after successful deletion
}

function EnvironmentDetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Skeleton className="h-9 w-32" />
      </div>

      <Card>
        <CardHeader>
          <Skeleton className="h-8 w-48" />
          <Skeleton className="h-5 w-96" />
        </CardHeader>
        <CardContent>
          <div className="space-y-6">
            <div>
              <Skeleton className="h-5 w-24 mb-4" />
              <div className="space-y-2">
                {[1, 2].map((i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            </div>

            <div>
              <Skeleton className="h-5 w-40 mb-4" />
              <div className="space-y-2">
                {[1, 2, 3].map((i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

const SUBDOMAIN_PATTERN = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/

function SubdomainCard({
  project,
  environment,
  onUpdate,
}: {
  project: ProjectResponse
  environment: EnvironmentResponse
  onUpdate: () => void
}) {
  const { data: settings } = useSettings()
  const previewDomain = (settings?.preview_domain ?? '').replace(/^\*\./, '')

  // The stored host label (e.g. "myproject-production"). Prefer the dedicated
  // `subdomain` field; if the API hasn't been updated yet, fall back to
  // extracting it from `main_url` by stripping the protocol and preview suffix.
  const currentLabel =
    (environment as EnvironmentResponse & { subdomain?: string }).subdomain ??
    environment.main_url
      .replace(/^https?:\/\//, '')
      .replace(new RegExp(`\\.${previewDomain.replace(/\./g, '\\.')}$`), '')

  const [value, setValue] = useState(currentLabel)

  const trimmed = value.trim().toLowerCase()
  const isUnchanged = trimmed === currentLabel
  const isValid = SUBDOMAIN_PATTERN.test(trimmed) && trimmed.length <= 63

  const mutation = useMutation({
    ...updateEnvironmentSubdomainMutation(),
    meta: { errorTitle: 'Failed to rename subdomain' },
    onSuccess: () => {
      toast.success(
        'Subdomain updated. The previous hostname will stop resolving shortly.'
      )
      onUpdate()
    },
  })

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!isValid || isUnchanged) return
    mutation.mutate({
      path: {
        project_id: project.id,
        env_id: environment.id,
      },
      body: { subdomain: trimmed },
    })
  }

  return (
    <SettingsSection
      title="Subdomain"
      icon={Globe}
      description="Rename the auto-managed hostname for this environment. The previous subdomain stops resolving as soon as the change is applied. Custom domains attached to this environment are unaffected."
    >
      <form onSubmit={handleSubmit} className="space-y-4">
        <div>
          <Label htmlFor="subdomain-input">Subdomain</Label>
          <div className="mt-2 flex flex-col gap-2 sm:flex-row sm:items-stretch">
            <div className="flex flex-1 items-stretch overflow-hidden rounded-md border border-input bg-background focus-within:ring-2 focus-within:ring-ring">
              <Input
                id="subdomain-input"
                value={value}
                onChange={(e) => setValue(e.target.value)}
                placeholder="e.g., myapp"
                className="rounded-none border-0 focus-visible:ring-0 focus-visible:ring-offset-0"
                maxLength={63}
                autoComplete="off"
                spellCheck={false}
              />
              {previewDomain && (
                <span className="flex items-center whitespace-nowrap border-l bg-muted/40 px-3 text-sm text-muted-foreground">
                  .{previewDomain}
                </span>
              )}
            </div>
            <Button
              type="submit"
              disabled={!isValid || isUnchanged || mutation.isPending}
              className="sm:w-auto"
            >
              {mutation.isPending && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Save
            </Button>
          </div>
          {!isValid && trimmed.length > 0 && (
            <p className="mt-2 text-xs text-destructive">
              Use 1–63 lowercase letters, digits, or hyphens. Cannot start or
              end with a hyphen.
            </p>
          )}
          {isValid && (
            <p className="mt-2 text-xs text-muted-foreground">
              DNS-safe slug: lowercase letters, digits, and hyphens (max 63
              chars). Casing is normalized server-side.
            </p>
          )}
        </div>
      </form>
    </SettingsSection>
  )
}

function PurgeAssetCacheCard({
  projectId,
  environmentId,
}: {
  projectId: number
  environmentId: number
}) {
  const [isPurging, setIsPurging] = useState(false)
  const [showConfirm, setShowConfirm] = useState(false)

  const handlePurge = async () => {
    setIsPurging(true)
    try {
      const response = await fetch(
        `/api/projects/${projectId}/environments/${environmentId}/asset-cache`,
        { method: 'DELETE' }
      )
      const data = await response.json()
      const deleted = data?.deleted ?? 0
      toast.success(`Purged ${deleted} cached asset${deleted !== 1 ? 's' : ''}`)
    } catch {
      toast.error('Failed to purge asset cache')
    } finally {
      setIsPurging(false)
      setShowConfirm(false)
    }
  }

  return (
    <SettingsSection
      title="Asset Cache"
      icon={HardDrive}
      description="Static assets (JS chunks, CSS, fonts) are cached for stale-chunk fallback. Purge if you need to force-clear cached assets for this environment."
    >
      <AlertDialog open={showConfirm} onOpenChange={setShowConfirm}>
        <AlertDialogTrigger asChild>
          <Button variant="outline" size="sm">
            <RefreshCw className="h-4 w-4 mr-2" />
            Purge Asset Cache
          </Button>
        </AlertDialogTrigger>
        <AlertDialogContent>
          <AlertDialogTitle>Purge Asset Cache</AlertDialogTitle>
          <AlertDialogDescription>
            This will delete all cached static assets for this environment.
            In-flight users with old HTML may see broken pages until they
            refresh. Orphaned blobs are cleaned up automatically overnight.
          </AlertDialogDescription>
          <div className="flex justify-end gap-3 mt-4">
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction onClick={handlePurge} disabled={isPurging}>
              {isPurging ? 'Purging...' : 'Purge Cache'}
            </AlertDialogAction>
          </div>
        </AlertDialogContent>
      </AlertDialog>
    </SettingsSection>
  )
}

export function EnvironmentDetail({
  project,
  environmentId: propEnvironmentId,
  initialEnvironment,
  onDelete,
}: EnvironmentDetailProps) {
  const { environmentId: paramEnvironmentId } = useParams<{
    environmentId: string
  }>()
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false)
  const queryClient = useQueryClient()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  // Use prop if provided, otherwise use URL param
  const environmentId = propEnvironmentId ?? Number(paramEnvironmentId)

  // Use the passed initialEnvironment if available, otherwise fetch
  const {
    data: environment = initialEnvironment,
    isLoading: isLoadingEnvironment,
    error: environmentError,
  } = useQuery({
    ...getEnvironmentOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId!),
      },
    }),
    initialData: initialEnvironment,
    staleTime: Infinity, // Keep initial data fresh indefinitely
    gcTime: 1000 * 60 * 10, // 10 minutes - keep in cache
    enabled: !initialEnvironment, // Only fetch if we don't have initial data
  })

  const removeEnvironmentMutation = useMutation({
    ...deleteEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment deleted successfully')
      setShowDeleteConfirm(false)
      queryClient.invalidateQueries({ queryKey: ['environments'] })

      // Call the onDelete callback if provided, otherwise fallback to history.back()
      if (onDelete) {
        onDelete()
      } else {
        window.history.back()
      }
    },
    onError: (error, variables) => {
      if (
        handleSensitiveActionError(error, () =>
          removeEnvironmentMutation.mutate(variables)
        )
      ) {
        setShowDeleteConfirm(false)
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(
        problem.detail || problem.message || 'Failed to delete environment'
      )
    },
  })

  if (isLoadingEnvironment) {
    return <EnvironmentDetailSkeleton />
  }

  if (environmentError) {
    return (
      <ErrorAlert
        title="Error loading environment"
        description={environmentError.message}
      />
    )
  }

  if (!environment) return null

  // Check if this is a production environment
  const isProduction = environment.slug === 'production'

  return (
    <div className="space-y-6">
      {verificationDialog}
      <EnvironmentConfigurationCard
        project={project}
        environment={environment}
        onUpdate={() => {
          queryClient.invalidateQueries({ queryKey: ['environment'] })
        }}
      />

      <SubdomainCard
        key={`${environment.id}:${
          (environment as EnvironmentResponse & { subdomain?: string })
            .subdomain ?? environment.main_url
        }`}
        project={project}
        environment={environment}
        onUpdate={() => {
          queryClient.invalidateQueries({ queryKey: ['environment'] })
          queryClient.invalidateQueries({ queryKey: ['environments'] })
        }}
      />

      <PurgeAssetCacheCard
        projectId={project.id}
        environmentId={environmentId}
      />

      <SettingsSection
        title="Danger Zone"
        icon={Trash2}
        description="Irreversible and destructive actions"
        className="border-destructive/50 bg-destructive/5"
      >
        <div className="space-y-4">
          <p className="text-sm text-muted-foreground">
            Deleting this environment will remove all configurations,
            deployments, and data associated with it. This action cannot be
            undone.
          </p>
          {isProduction && (
            <p className="text-sm text-muted-foreground bg-muted p-3 rounded-md border">
              ℹ️ The production environment cannot be deleted to prevent
              accidental data loss.
            </p>
          )}
          <AlertDialog
            open={showDeleteConfirm}
            onOpenChange={setShowDeleteConfirm}
          >
            <AlertDialogTrigger asChild>
              <Button
                variant="destructive"
                disabled={isProduction}
                className="w-full sm:w-auto"
              >
                <Trash2 className="h-4 w-4 mr-2" />
                Delete Environment
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogTitle>Delete Environment</AlertDialogTitle>
              <AlertDialogDescription>
                Are you sure you want to delete the &quot;{environment.name}
                &quot; environment? This action cannot be undone.
              </AlertDialogDescription>
              <div className="flex justify-end gap-3 mt-6">
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  onClick={async () => {
                    await removeEnvironmentMutation.mutateAsync({
                      path: {
                        project_id: project.id || 0,
                        env_id: Number(environmentId),
                      },
                    })
                  }}
                  className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                  disabled={removeEnvironmentMutation.isPending}
                >
                  {removeEnvironmentMutation.isPending
                    ? 'Deleting...'
                    : 'Delete Environment'}
                </AlertDialogAction>
              </div>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </SettingsSection>
    </div>
  )
}

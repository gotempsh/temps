// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentsOptions,
  createEnvironmentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Route, Routes, useSearchParams } from 'react-router'
import {
  EnvironmentDashboard,
  EnvironmentDashboardSkeleton,
} from './EnvironmentDashboard'
import { ContainerDetailPage } from './ContainerDetailPage'
import { ProjectResponse } from '@/api/client'
import { CreateEnvironmentDialog } from '@/components/project/settings/environments/CreateEnvironmentDialog'
import { Button } from '@/components/ui/button'
import { PageState } from '@temps-sdk/ds'
import { Layers, Plus } from 'lucide-react'
import { toast } from 'sonner'
import {
  resolveEnvironmentSelection,
  updateEnvironmentSearchParams,
} from '@/lib/environment-navigation'

export function EnvironmentsTabsView({
  project,
  selectedEnvironmentId,
}: {
  project: ProjectResponse
  /** The environment picked in the project bar. */
  selectedEnvironmentId?: number
}) {
  const [searchParams, setSearchParams] = useSearchParams()
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)

  const {
    data: environments,
    isLoading: isEnvironmentsLoading,
    refetch: refetchEnvironments,
  } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project?.id || 0,
      },
    }),
    enabled: !!project?.id,
  })

  const createEnv = useMutation({
    ...createEnvironmentMutation(),
    onSuccess: () => {
      toast.success('Environment created successfully')
      refetchEnvironments()
      setIsCreateDialogOpen(false)
    },
    onError: (error: Error) => {
      toast.error(error.message || 'Failed to create environment')
    },
  })

  const activeEnvId =
    selectedEnvironmentId ??
    resolveEnvironmentSelection(
      searchParams.get('environment'),
      environments ?? []
    )

  if (isEnvironmentsLoading) {
    return <EnvironmentDashboardSkeleton />
  }

  if (!environments || environments.length === 0) {
    return (
      <div className="flex flex-col h-full">
        <PageState
          variant="empty"
          size="compact"
          icon={Layers}
          title="No environments"
          description="Environments are where this service runs: production, staging or a preview."
          action={
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="size-4" aria-hidden="true" />
              New environment
            </Button>
          }
        />
        <CreateEnvironmentDialog
          open={isCreateDialogOpen}
          onOpenChange={setIsCreateDialogOpen}
          project={project}
          onSubmit={async (values) => {
            await createEnv.mutateAsync({
              path: { project_id: project.id || 0 },
              body: values,
            })
          }}
        />
      </div>
    )
  }

  if (!activeEnvId) return null

  const handleDelete = async (deletedId: number) => {
    const remaining = environments.filter((e) => e.id !== deletedId)
    await refetchEnvironments()
    const nextParams = updateEnvironmentSearchParams(searchParams, {
      environmentId: remaining[0]?.id ?? null,
    })
    setSearchParams(nextParams, { replace: true })
  }

  return (
    <div className="flex flex-col h-full">
      <Routes>
        <Route
          path="containers/:containerId"
          element={<ContainerDetailPage project={project} />}
        />
        <Route
          path="*"
          element={
            <EnvironmentDashboard
              key={activeEnvId}
              project={project}
              environmentId={activeEnvId}
              onCreateEnvironment={() => setIsCreateDialogOpen(true)}
              onDelete={() => handleDelete(activeEnvId)}
            />
          }
        />
      </Routes>
      <CreateEnvironmentDialog
        open={isCreateDialogOpen}
        onOpenChange={setIsCreateDialogOpen}
        project={project}
        onSubmit={async (values) => {
          await createEnv.mutateAsync({
            path: { project_id: project.id || 0 },
            body: values,
          })
        }}
      />
    </div>
  )
}

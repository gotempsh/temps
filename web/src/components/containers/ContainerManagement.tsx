import { useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useContainers } from '@/hooks/containers'
import { ContainerList } from './ContainerList'
import { ContainerDetail } from './ContainerDetail'
import { ContainerActionDialog } from './ContainerActionDialog'

interface ContainerManagementProps {
  projectId: string
  environmentId: string
}

export function ContainerManagement({
  projectId,
  environmentId,
}: ContainerManagementProps) {
  const [searchParams, setSearchParams] = useSearchParams()
  const [actionType, setActionType] = useState<
    'start' | 'stop' | 'restart' | null
  >(null)

  const { data: containers, isLoading } = useContainers(
    projectId,
    environmentId
  )

  // Get container ID from URL params or default to first container
  const userSelectedId = searchParams.get('container')
  const selectedContainerId =
    userSelectedId ?? containers?.containers?.[0]?.container_id ?? null

  // Get tab from URL params or default to 'overview'
  const selectedTab =
    (searchParams.get('tab') as 'overview' | 'logs' | 'configuration' | null) ??
    'overview'

  // Handle container selection with URL update
  const handleSelectContainer = (id: string) => {
    searchParams.set('container', id)
    searchParams.set('tab', 'overview') // Reset tab when changing container
    setSearchParams(searchParams)
  }

  // Handle tab change with URL update
  const handleTabChange = (tab: 'overview' | 'logs' | 'configuration') => {
    searchParams.set('tab', tab)
    setSearchParams(searchParams)
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-96">
        <p className="text-muted-foreground">Loading containers...</p>
      </div>
    )
  }

  if (!containers || containers?.containers?.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-96 border rounded-lg bg-gradient-to-b from-muted/40 to-muted/20 p-6">
        <div className="text-center space-y-2">
          <p className="text-sm font-semibold text-foreground">
            No containers yet
          </p>
          <p className="text-sm text-muted-foreground">
            This environment doesn&apos;t have any running containers
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex gap-4 h-full rounded-lg overflow-hidden bg-background">
      {/* Container List Sidebar */}
      <div className="flex-shrink-0 overflow-y-auto">
        <ContainerList
          containers={containers?.containers}
          selectedId={selectedContainerId}
          onSelect={handleSelectContainer}
        />
      </div>

      {/* Container Detail - Always show since first is selected by default */}
      <div className="flex-1 overflow-hidden">
        {selectedContainerId && (
          <ContainerDetail
            projectId={projectId}
            environmentId={environmentId}
            containerId={selectedContainerId}
            tab={selectedTab}
            onTabChange={handleTabChange}
            onAction={setActionType}
          />
        )}
      </div>

      {/* Action Confirm Dialog */}
      <ContainerActionDialog
        projectId={projectId}
        environmentId={environmentId}
        action={actionType}
        containerId={selectedContainerId}
        onClose={() => setActionType(null)}
      />
    </div>
  )
}

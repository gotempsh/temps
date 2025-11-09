import { getProjectBySlugOptions, getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Card } from '@/components/ui/card'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { useNavigate, useParams } from 'react-router-dom'

export function EnvironmentsList() {
  const { slug } = useParams()
  const navigate = useNavigate()

  // First get project by slug
  const { data: project, isLoading: isProjectLoading } = useQuery({
    ...getProjectBySlugOptions({
      path: {
        slug: slug || '',
      },
    }),
    enabled: !!slug,
  })

  // Then get environments
  const { data: environments, isLoading: isEnvironmentsLoading } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project?.id || 0,
      },
    }),
    enabled: !!project?.id,
  })

  const isLoading = isProjectLoading || isEnvironmentsLoading

  if (isLoading) {
    return (
      <div className="flex flex-col h-full">
        <div className="p-6 border-b bg-background">
          <div>
            <Skeleton className="h-8 w-48 mb-2" />
            <Skeleton className="h-4 w-64" />
          </div>
        </div>
        <div className="flex-1 p-6">
          <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
            {Array.from({ length: 6 }).map((_, i) => (
              <Skeleton key={i} className="h-32 w-full" />
            ))}
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="p-6 border-b bg-background">
        <h1 className="text-3xl font-bold">Environments</h1>
        <p className="text-sm text-muted-foreground mt-1">
          Select an environment to view its dashboard and manage containers
        </p>
      </div>

      {/* Environments Grid */}
      <div className="flex-1 overflow-auto">
        <div className="p-6">
          {!environments || environments.length === 0 ? (
            <div className="flex items-center justify-center h-96 border rounded-lg bg-muted/20">
              <p className="text-muted-foreground">No environments found</p>
            </div>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4 max-w-6xl">
              {environments.map((env) => (
                <EnvironmentCard
                  key={env.id}
                  environment={env}
                  onSelect={() => navigate(`/projects/${slug}/environments/${env.id}`)}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

interface EnvironmentCardProps {
  environment: any
  onSelect: () => void
}

function EnvironmentCard({ environment, onSelect }: EnvironmentCardProps) {
  const statusColor = {
    running: 'bg-green-500',
    stopped: 'bg-gray-400',
    error: 'bg-red-500',
  }[environment.status] || 'bg-gray-400'

  return (
    <Card
      className="p-6 cursor-pointer transition hover:shadow-lg hover:border-primary/50"
      onClick={onSelect}
    >
      <div className="space-y-4">
        {/* Header */}
        <div className="flex items-start justify-between">
          <div>
            <h3 className="text-lg font-semibold">{environment.name}</h3>
            <p className="text-sm text-muted-foreground mt-1">
              Branch: <code className="bg-muted px-1.5 py-0.5 rounded text-xs">{environment.branch}</code>
            </p>
          </div>
          <Badge
            variant={environment.status === 'running' ? 'default' : 'secondary'}
          >
            {environment.status}
          </Badge>
        </div>

        {/* Status Indicator */}
        <div className="flex items-center gap-2">
          <div className={`w-2 h-2 rounded-full ${statusColor}`} />
          <span className="text-sm text-muted-foreground">
            {environment.status === 'running'
              ? 'Environment is running'
              : 'Environment is not running'}
          </span>
        </div>

        {/* Click hint */}
        <div className="text-xs text-muted-foreground pt-2 border-t">
          Click to view dashboard
        </div>
      </div>
    </Card>
  )
}

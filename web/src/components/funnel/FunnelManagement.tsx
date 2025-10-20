import { ProjectResponse, FunnelResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  listFunnelsOptions,
  deleteFunnelMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { BarChart3, Plus } from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { FunnelCard } from './FunnelCard'

interface FunnelManagementProps {
  project: ProjectResponse
}

export function FunnelManagement({ project }: FunnelManagementProps) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const {
    data: funnels,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listFunnelsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const deleteFunnel = useMutation({
    ...deleteFunnelMutation(),
    meta: {
      errorTitle: 'Failed to delete funnel',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listFunnels'] })
    },
  })

  const handleDelete = async (funnelId: number) => {
    if (confirm('Are you sure you want to delete this funnel?')) {
      deleteFunnel.mutate({
        path: {
          project_id: project.id,
          funnel_id: funnelId,
        },
      })
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-semibold">Funnels</h2>
          <p className="text-muted-foreground">
            Track user conversion through defined steps
          </p>
        </div>
        <Button
          onClick={() =>
            navigate(`/projects/${project.slug}/analytics/funnels/create`)
          }
        >
          <Plus className="h-4 w-4 mr-2" />
          Create Funnel
        </Button>
      </div>

      {isLoading ? (
        <div className="grid gap-4">
          {Array.from({ length: 3 }).map((_, i) => (
            <Card key={i}>
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="space-y-2">
                    <Skeleton className="h-5 w-32" />
                    <Skeleton className="h-3 w-48" />
                  </div>
                  <div className="flex items-center gap-2">
                    <Skeleton className="h-8 w-8" />
                    <Skeleton className="h-8 w-8" />
                  </div>
                </div>
              </CardHeader>
              <CardContent>
                <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
                  {Array.from({ length: 3 }).map((_, j) => (
                    <div key={j} className="flex items-center gap-2">
                      <Skeleton className="h-4 w-4 rounded-full" />
                      <div className="space-y-1">
                        <Skeleton className="h-3 w-20" />
                        <Skeleton className="h-5 w-12" />
                      </div>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-8">
            <p className="text-muted-foreground mb-2">Failed to load funnels</p>
            <Button variant="outline" onClick={() => refetch()}>
              Try again
            </Button>
          </CardContent>
        </Card>
      ) : !funnels?.length ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <BarChart3 className="h-12 w-12 text-muted-foreground mb-4" />
            <h3 className="text-lg font-semibold mb-2">No funnels created</h3>
            <p className="text-muted-foreground mb-4">
              Create your first funnel to track user conversion
            </p>
            <Button
              onClick={() =>
                navigate(`/projects/${project.slug}/analytics/funnels/create`)
              }
            >
              <Plus className="h-4 w-4 mr-2" />
              Create Funnel
            </Button>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4">
          {funnels.map((funnel: FunnelResponse) => (
            <FunnelCard
              key={funnel.id}
              funnel={funnel}
              project={project}
              onDelete={() => handleDelete(funnel.id)}
              onView={() =>
                navigate(
                  `/projects/${project.slug}/analytics/funnels/${funnel.id}`
                )
              }
              onEdit={() =>
                navigate(
                  `/projects/${project.slug}/analytics/funnels/${funnel.id}/edit`
                )
              }
            />
          ))}
        </div>
      )}
    </div>
  )
}

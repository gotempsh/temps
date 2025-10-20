import { ProjectResponse, FunnelResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { getFunnelMetricsOptions } from '@/api/client/@tanstack/react-query.gen'
import { formatDateForAPI } from '@/lib/date'
import { useQuery } from '@tanstack/react-query'
import { subDays } from 'date-fns'
import { Users, TrendingUp, Clock, Trash2, Pencil } from 'lucide-react'

interface FunnelCardProps {
  funnel: FunnelResponse
  project: ProjectResponse
  onDelete: () => void
  onView: () => void
  onEdit: () => void
}

export function FunnelCard({
  funnel,
  project,
  onDelete,
  onView,
  onEdit,
}: FunnelCardProps) {
  const { data: metrics, isLoading: metricsLoading } = useQuery({
    ...getFunnelMetricsOptions({
      path: {
        project_id: project.id,
        funnel_id: funnel.id,
      },
      query: {
        start_date: formatDateForAPI(subDays(new Date(), 30)),
        end_date: formatDateForAPI(new Date()),
      },
    }),
  })

  return (
    <Card
      className="cursor-pointer transition-all hover:shadow-md hover:border-primary/50"
      onClick={onView}
    >
      <CardHeader>
        <div className="flex items-start justify-between">
          <div>
            <CardTitle className="text-lg">{funnel.name}</CardTitle>
            {funnel.description && (
              <CardDescription className="mt-1">
                {funnel.description}
              </CardDescription>
            )}
          </div>
          <div className="flex gap-1">
            <Button
              variant="ghost"
              size="sm"
              onClick={(e) => {
                e.stopPropagation()
                onEdit()
              }}
            >
              <Pencil className="h-4 w-4" />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={(e) => {
                e.stopPropagation()
                onDelete()
              }}
            >
              <Trash2 className="h-4 w-4 text-destructive" />
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent>
        {metricsLoading ? (
          <div className="grid grid-cols-3 gap-4">
            {Array.from({ length: 3 }).map((_, i) => (
              <div key={i} className="space-y-2">
                <div className="h-3 bg-muted rounded w-16" />
                <div className="h-6 bg-muted rounded w-12" />
              </div>
            ))}
          </div>
        ) : metrics ? (
          <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
            <div className="flex items-center gap-2">
              <Users className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm text-muted-foreground">Total Entries</p>
                <p className="text-lg font-semibold">
                  {metrics.total_entries.toLocaleString()}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <TrendingUp className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm text-muted-foreground">Conversion Rate</p>
                <p className="text-lg font-semibold">
                  {metrics.overall_conversion_rate.toFixed(1)}%
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Clock className="h-4 w-4 text-muted-foreground" />
              <div>
                <p className="text-sm text-muted-foreground">
                  Avg. Completion Time
                </p>
                <p className="text-lg font-semibold">
                  {Math.round(metrics.average_completion_time_seconds / 60)}m
                </p>
              </div>
            </div>
          </div>
        ) : (
          <p className="text-muted-foreground">No data available</p>
        )}
      </CardContent>
    </Card>
  )
}

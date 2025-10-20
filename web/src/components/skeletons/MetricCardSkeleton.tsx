import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'

export function MetricCardSkeleton() {
  return (
    <Card>
      <CardContent className="pt-6">
        <div className="flex items-center justify-between space-y-0 pb-2">
          <Skeleton className="h-5 w-20" />
          <Skeleton className="h-5 w-5" />
        </div>
        <div className="space-y-2">
          <Skeleton className="h-7 w-24" />
          <Skeleton className="h-4 w-32" />
        </div>
      </CardContent>
    </Card>
  )
}

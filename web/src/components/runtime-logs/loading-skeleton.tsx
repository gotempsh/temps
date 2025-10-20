import { Skeleton } from '@/components/ui/skeleton'

export function LogViewerSkeleton() {
  return (
    <div className="space-y-4 p-4">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="space-y-2">
            <Skeleton className="h-4 w-20" />
            <Skeleton className="h-10 w-full" />
          </div>
        ))}
      </div>
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-[600px] w-full" />
    </div>
  )
}

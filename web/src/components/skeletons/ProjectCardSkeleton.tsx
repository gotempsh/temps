import { Skeleton } from '@/components/ui/skeleton'

export function ProjectCardSkeleton() {
  return (
    <div className="grid gap-4 px-4 py-3.5 lg:grid-cols-[minmax(14rem,0.8fr)_minmax(30rem,1.7fr)_minmax(11rem,0.65fr)_2.5rem] lg:items-center">
      <div className="flex items-center gap-3">
        <Skeleton className="size-9 shrink-0 rounded-md" />
        <div className="space-y-1.5">
          <Skeleton className="h-4 w-36" />
          <Skeleton className="h-3 w-24" />
        </div>
      </div>
      <div className="grid grid-cols-2 gap-6 xl:grid-cols-3">
        <Skeleton className="h-8 w-28" />
        <Skeleton className="h-8 w-28" />
        <Skeleton className="h-8 w-36" />
      </div>
      <div className="space-y-1.5">
        <Skeleton className="h-5 w-20" />
        <Skeleton className="h-3 w-24" />
      </div>
      <Skeleton className="hidden h-5 w-9 lg:block" />
    </div>
  )
}

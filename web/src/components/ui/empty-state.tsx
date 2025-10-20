import { LucideIcon } from 'lucide-react'
import { ReactNode } from 'react'

interface EmptyStateProps {
  icon: LucideIcon
  title: string
  description: ReactNode | string
  action?: ReactNode
}

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
}: EmptyStateProps) {
  return (
    <div className="flex min-h-[400px] flex-col items-center justify-center gap-4 rounded-lg p-8 text-center animate-in fade-in-50">
      <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
        <Icon className="h-10 w-10 text-muted-foreground" />
      </div>
      <div className="max-w-md space-y-2">
        <h3 className="text-lg font-semibold">{title}</h3>
        {typeof description === 'string' ? (
          <p className="text-sm text-muted-foreground">{description}</p>
        ) : (
          description
        )}
      </div>
      {action}
    </div>
  )
}

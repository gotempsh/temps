import { LucideIcon } from 'lucide-react'
import { cn } from '@/lib/utils'

interface EmptyPlaceholderProps extends React.HTMLAttributes<HTMLDivElement> {
  icon?: LucideIcon
  title: string
  description: string
  action?: React.ReactNode
}

export function EmptyPlaceholder({
  icon: Icon,
  title,
  description,
  action,
  className,
  ...props
}: EmptyPlaceholderProps) {
  return (
    <div
      className={cn(
        'flex min-h-[400px] flex-col items-center justify-center rounded-md p-8 text-center animate-in fade-in-50',
        className
      )}
      {...props}
    >
      {Icon && (
        <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
          <Icon className="h-10 w-10" />
        </div>
      )}
      <h3 className="mt-4 text-lg font-semibold">{title}</h3>
      <p className="mt-2 mb-4 text-sm text-muted-foreground">{description}</p>
      {action}
    </div>
  )
}

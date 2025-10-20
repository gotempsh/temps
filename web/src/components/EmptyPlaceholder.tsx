import { LucideIcon } from 'lucide-react'

interface EmptyPlaceholderProps {
  icon?: LucideIcon
  title: string
  description: string
  children?: React.ReactNode
}

export function EmptyPlaceholder({
  icon: Icon,
  title,
  description,
  children,
}: EmptyPlaceholderProps) {
  return (
    <div className="flex min-h-[400px] flex-col items-center justify-center rounded-md border border-dashed p-8 text-center animate-in fade-in-50">
      {Icon && (
        <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
          <Icon className="h-10 w-10 text-muted-foreground" />
        </div>
      )}
      <h2 className="mt-6 text-xl font-semibold">{title}</h2>
      <p className="mt-2 text-center text-sm font-normal leading-6 text-muted-foreground">
        {description}
      </p>
      {children && <div className="mt-6">{children}</div>}
    </div>
  )
}

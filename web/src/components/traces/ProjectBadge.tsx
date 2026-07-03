import { cn } from '@/lib/utils'

// Deterministic per-project colour so a project keeps the same hue across the
// legend, its span badges, and its waterfall rows. Shared by the single-project
// trace detail (`TraceDetail`) and the standalone unified view
// (`CrossProjectTraceDetail`) so a project looks identical in both.
const PROJECT_COLORS = [
  '#6366f1', // indigo
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ec4899', // pink
  '#06b6d4', // cyan
  '#8b5cf6', // violet
  '#ef4444', // red
  '#14b8a6', // teal
  '#0ea5e9', // sky
  '#a855f7', // purple
]

export function projectColor(projectId: number): string {
  return PROJECT_COLORS[Math.abs(projectId) % PROJECT_COLORS.length]
}

/** A colour-matched project badge used in the legend and on each span row. */
export function ProjectBadge({
  projectId,
  name,
  className,
}: {
  projectId: number
  name: string
  className?: string
}) {
  const color = projectColor(projectId)
  return (
    <span
      className={cn(
        'inline-flex max-w-[120px] items-center gap-1 truncate rounded px-1.5 py-0.5 text-[10px] font-medium',
        className
      )}
      style={{ backgroundColor: `${color}22`, color }}
      title={name}
    >
      <span
        className="h-2 w-2 shrink-0 rounded-full"
        style={{ backgroundColor: color }}
      />
      {name}
    </span>
  )
}

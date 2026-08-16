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

/** A colour-matched project badge used in the legend and in detail panels. */
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
        'inline-flex max-w-[88px] shrink-0 items-center gap-1 truncate rounded px-1.5 py-0.5 text-[10px] font-medium',
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

/**
 * Just the colour, for per-span use in the waterfall.
 *
 * Span rows are the one place the full badge does not pay for itself: the name
 * column is already competing with indentation and the span name, so a slug
 * like `payments-gateway-prod` truncates to `payments-gatew…` on every row and
 * still costs ~88px. The colour carries the identity and `ProjectLegend`
 * decodes it once at the top; the name stays reachable via the tooltip.
 */
export function ProjectDot({
  projectId,
  name,
  className,
}: {
  projectId: number
  name: string
  className?: string
}) {
  return (
    <span
      className={cn('h-2.5 w-2.5 shrink-0 rounded-full', className)}
      style={{ backgroundColor: projectColor(projectId) }}
      title={name}
      // role="img" so the label is actually announced — an aria-label on a
      // role-less generic element is ignored by most screen readers, which
      // would leave the project unreadable once the slug text is gone.
      role="img"
      aria-label={`Project: ${name}`}
    />
  )
}

/** Decodes the per-span dot colours. Required wherever `ProjectDot` is used. */
export function ProjectLegend({
  projects,
  className,
}: {
  projects: Array<{ project_id: number; project_name: string }>
  className?: string
}) {
  if (projects.length === 0) return null
  return (
    <div className={cn('flex flex-wrap items-center gap-2', className)}>
      <span className="text-xs text-muted-foreground">Projects:</span>
      {projects.map((p) => (
        <ProjectBadge
          key={p.project_id}
          projectId={p.project_id}
          name={p.project_name}
          // The legend is what decodes the dots, so it shows the whole slug —
          // truncating here would defeat the point.
          className="max-w-none"
        />
      ))}
    </div>
  )
}

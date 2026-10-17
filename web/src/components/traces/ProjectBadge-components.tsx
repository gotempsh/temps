// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { projectColor } from './ProjectBadge-shared'

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
        'inline-flex max-w-[88px] shrink-0 items-center gap-1 truncate rounded px-1.5 py-0.5 text-[10px] font-medium text-foreground',
        className
      )}
      // The hue only tints the surface and the dot. Rendering the name in the
      // hue itself put pastel text (emerald, amber, ...) on its own 13% tint
      // at ~2:1, so the text uses the foreground token and the dot carries
      // the colour identity.
      style={{ backgroundColor: `${color}22` }}
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
      aria-label={`Service: ${name}`}
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
      <span className="text-xs text-muted-foreground">Services:</span>
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

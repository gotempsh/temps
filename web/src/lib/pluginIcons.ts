import { icons, type LucideIcon } from 'lucide-react'

/**
 * Resolve a Lucide icon name string (kebab-case or lowercase) to a LucideIcon component.
 * Falls back to the Puzzle icon if the name is not found.
 *
 * Examples:
 *   resolvePluginIcon("database") -> Database
 *   resolvePluginIcon("bar-chart-3") -> BarChart3
 *   resolvePluginIcon("puzzle") -> Puzzle
 *   resolvePluginIcon("unknown-icon") -> Puzzle (fallback)
 */
export function resolvePluginIcon(name: string): LucideIcon {
  // lucide-react exports icons with PascalCase keys (e.g., "BarChart3", "Database")
  // Plugin manifests use kebab-case (e.g., "bar-chart-3", "database")
  // Convert kebab-case to PascalCase for lookup
  const pascalCase = name
    .split('-')
    .map((segment) => {
      if (!segment) return ''
      return segment.charAt(0).toUpperCase() + segment.slice(1)
    })
    .join('')

  const icon = (icons as Record<string, LucideIcon>)[pascalCase]
  if (icon) return icon

  // Also try the raw name (in case it's already PascalCase)
  const directIcon = (icons as Record<string, LucideIcon>)[name]
  if (directIcon) return directIcon

  // Fallback to Puzzle icon
  return icons.Puzzle
}

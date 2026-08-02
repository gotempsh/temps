/**
 * The platforms most users migrate in from.
 *
 * Used by the "Migrate from" strip in the projects page header — the single
 * migration entry point for a fresh instance. `source` ids must match the
 * backend `ImportSource` values; each one deep-links the import wizard with
 * the source preselected. The full list (Portainer, Kamal, …) lives on the
 * wizard itself, which the strip links to as a fallback.
 */
export const TOP_MIGRATION_SOURCES: ReadonlyArray<{
  source: string
  label: string
}> = [
  { source: 'coolify', label: 'Coolify' },
  { source: 'dokploy', label: 'Dokploy' },
  { source: 'caprover', label: 'CapRover' },
  { source: 'kubernetes', label: 'Kubernetes' },
  { source: 'docker', label: 'Docker' },
] as const

/** Deep link into the import wizard with `source` preselected. */
export function importHref(source: string): string {
  return `/projects/import-wizard?source=${source}`
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  generatedKeys,
  typedVariables,
  type DetectedVar,
  type VarRow,
} from './env'

/** What detection said a database would give the app (`databases` in the env-example response). */
export interface DatabaseOptionLike {
  serviceType: string
  label: string
  suggested: boolean
  reason?: string | null
  /** Names the service injects itself. */
  provides: string[]
  /** The app's name → the injected one, for names the app asks for. */
  envAliases: Record<string, string>
}

/** The env-example response's variable, in the shape the form reads. */
export function detectedFrom(
  variables: Array<{
    key: string
    defaultValue?: string | null
    looksSecret?: boolean
    canGenerate?: boolean
  }>
): DetectedVar[] {
  return variables.map((v) => ({
    key: v.key,
    example_value: v.defaultValue ?? '',
    looks_secret: v.looksSecret,
    can_generate: v.canGenerate,
  }))
}

/** The names a chosen database fills: what it injects, or what an alias maps onto it, that the app asks for. */
export function providedNames(
  option: DatabaseOptionLike | null,
  detected: DetectedVar[]
): string[] {
  if (!option) return []
  const fills = new Set([...option.provides, ...Object.keys(option.envAliases)])
  return detected.map((d) => d.key).filter((key) => fills.has(key))
}

/** The suggested database, if detection found one. */
export function suggestedDatabase<T extends DatabaseOptionLike>(
  options: T[]
): T | null {
  return options.find((o) => o.suggested) ?? null
}

/** A project name from the repository's: lowercase letters, digits and dashes. */
export function projectNameFrom(repoName: string): string {
  const name = repoName
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
  return name.slice(0, 40) || 'app'
}

/** The preset at the repository's root, else the first one detected. */
export function rootPreset<T extends { path: string }>(presets: T[]): T | null {
  return (
    presets.find((p) => ['', '.', './', '/'].includes(p.path)) ??
    presets[0] ??
    null
  )
}

export type ProjectVariable =
  | { key: string; value: string; is_secret: boolean }
  | { key: string; generate: { kind: 'secret'; bytes: number } }

/**
 * The variables the project is created with: typed and example values as
 * sent, and generated secrets as a request the server fulfils, so they never
 * pass through the browser and exist before the first deploy.
 */
export function projectVariables(rows: VarRow[]): ProjectVariable[] {
  return [
    ...typedVariables(rows),
    ...generatedKeys(rows).map((key) => ({
      key,
      generate: { kind: 'secret' as const, bytes: 32 },
    })),
  ]
}

/** The aliases to set on the new database's link, keyed by its id; none when it has none. */
export function linkAliases(
  serviceId: number | null,
  option: DatabaseOptionLike | null
): Record<string, Record<string, string>> | undefined {
  if (
    serviceId == null ||
    !option ||
    Object.keys(option.envAliases).length === 0
  )
    return undefined
  return { [String(serviceId)]: option.envAliases }
}

/**
 * What the ship step saw when it opened: the projects listed, and the moment
 * a project must be created after to count as new.
 */
export interface ProjectsBaseline {
  ids: Set<number>
  /** Milliseconds since the epoch, on the server's clock. */
  since: number
}

/**
 * The baseline from the step's first listing. The list is ordered by last
 * deployment, so redeploying an older project can bring it onto the page:
 * not being listed before does not make a project new, being created after
 * the step opened does. `serverDate` is that response's `Date` header, the
 * server's clock, so a skewed browser clock can neither hide the agent's
 * project nor invent one; without it, the newest project listed.
 */
export function projectsBaseline(
  list: { id: number; created_at: string }[],
  serverDate: string | null
): ProjectsBaseline {
  const opened = serverDate ? Date.parse(serverDate) : Number.NaN
  const newest = Math.max(
    Number.NEGATIVE_INFINITY,
    ...list.map((p) => Date.parse(p.created_at)).filter(Number.isFinite)
  )
  return {
    ids: new Set(list.map((p) => p.id)),
    since: Number.isFinite(opened) ? opened : newest,
  }
}

/** Projects created since the step opened, in the order listed. */
export function newProjects<T extends { id: number; created_at: string }>(
  baseline: ProjectsBaseline | null,
  list: T[] | undefined
): T[] {
  if (!baseline || !list) return []
  return list.filter(
    (p) => !baseline.ids.has(p.id) && Date.parse(p.created_at) >= baseline.since
  )
}

/** Deployment states in which an app is live: serving now, or a static build that finished. */
/**
 * Whether one of a project's environments is serving a deployment. Opening a
 * project from the step marks the app step done only then: an agent may
 * create a project and fail to upload its source, or its first build may
 * fail. An environment's current deployment is set only when one completes,
 * so this holds however many failed after it, which a page of the
 * deployment history cannot tell.
 */
export function hasLiveEnvironment(
  environments: { current_deployment_id?: number | null }[]
): boolean {
  return environments.some((e) => e.current_deployment_id != null)
}

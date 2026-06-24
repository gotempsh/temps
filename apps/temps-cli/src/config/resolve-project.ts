import { config } from './store.js'
import { readProjectConfig } from './project-config.js'
import { getActiveDefaultProjectSync, setActiveDefaultProject } from './contexts.js'

export type ProjectSource = 'flag' | 'local-config' | 'env-var' | 'context-default' | 'global-config'

export interface ResolvedProject {
  slug: string
  source: ProjectSource
}

/**
 * Resolve the project slug using priority chain:
 * CLI flag > .temps/config.json > TEMPS_PROJECT env var
 *   > active context's defaultProject > legacy global defaultProject
 */
export async function resolveProjectSlug(flagValue?: string): Promise<ResolvedProject | null> {
  // 1. CLI flag takes highest priority
  if (flagValue) {
    return { slug: flagValue, source: 'flag' }
  }

  // 2. Local .temps/config.json
  const localConfig = await readProjectConfig()
  if (localConfig?.projectSlug) {
    return { slug: localConfig.projectSlug, source: 'local-config' }
  }

  // 3. TEMPS_PROJECT environment variable
  const envProject = process.env.TEMPS_PROJECT
  if (envProject) {
    return { slug: envProject, source: 'env-var' }
  }

  // 4. Active context's default project (scoped per Temps server).
  const contextDefault = getActiveDefaultProjectSync()
  if (contextDefault) {
    return { slug: contextDefault, source: 'context-default' }
  }

  // 5. Legacy global default project. Kept for back-compat with configs
  //    written before defaults were scoped per context. New writes go to the
  //    active context, so this drains over time.
  const defaultProject = config.get('defaultProject')
  if (defaultProject) {
    return { slug: defaultProject, source: 'global-config' }
  }

  return null
}

/**
 * Persist the default project slug. Writes to the active context so the
 * default is scoped to the Temps server it belongs to. When there's no
 * active context (e.g. env-var-only auth), falls back to the legacy global
 * key so the behaviour still works. Pass `undefined` to clear it.
 */
export async function setDefaultProject(slug: string | undefined): Promise<void> {
  const wroteToContext = await setActiveDefaultProject(slug)
  if (!wroteToContext) {
    config.set('defaultProject', slug)
  }
}

/**
 * Resolve project slug or throw with a helpful message
 */
export async function requireProjectSlug(flagValue?: string): Promise<ResolvedProject> {
  const resolved = await resolveProjectSlug(flagValue)
  if (resolved) return resolved

  throw new Error(
    'No project specified. You can:\n' +
    '  • Pass --project <slug> flag\n' +
    '  • Run "temps link" to link this directory to a project\n' +
    '  • Set TEMPS_PROJECT environment variable\n' +
    '  • Run "temps configure" to set a default project'
  )
}

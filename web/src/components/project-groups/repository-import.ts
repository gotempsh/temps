// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ImportedApp,
  InstantiationResponse,
  PresetInfo,
  ProjectPresetResponse,
} from '@/api/client'

/** An app found by the public-repository scan, in the connected scan's shape. */
export function fromPublicPreset(p: PresetInfo): ProjectPresetResponse {
  return {
    path: p.path,
    preset: p.preset,
    presetLabel: p.preset_label,
    projectType: p.project_type,
    exposedPort: p.exposed_port,
    dockerfilePath: p.dockerfile_path,
    composeFiles: p.compose_files,
    iconUrl: p.icon_url,
  }
}

/** `./apps/web/` → `apps/web`; the repository root is `.`. */
export function normalizeDirectory(path: string | null | undefined): string {
  const trimmed = (path ?? '')
    .trim()
    .replace(/^\.\/+/, '')
    .replace(/\/+$/, '')
  return trimmed === '' || trimmed === '.' ? '.' : trimmed
}

/**
 * A readable service name for an app found at `path`: its folder name, or
 * the repository's name for an app at the root. Suffixed when taken.
 */
export function suggestServiceName(
  path: string,
  repoName: string,
  taken: ReadonlySet<string>
): string {
  const dir = normalizeDirectory(path)
  const base =
    (dir === '.' ? repoName : dir.split('/').pop() || repoName)
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-')
      .replace(/^-+|-+$/g, '') || 'service'
  if (!taken.has(base)) return base
  for (let i = 2; ; i++) {
    const candidate = `${base}-${i}`
    if (!taken.has(candidate)) return candidate
  }
}

/**
 * An app as the import endpoint takes it: the service builds from the app's
 * directory of the project's repository and follows its branch.
 */
export function importedApp(
  app: ProjectPresetResponse,
  name: string
): ImportedApp {
  const preset = app.preset
  return {
    name: name.trim(),
    directory: normalizeDirectory(app.path),
    preset,
    exposed_port: app.exposedPort ?? undefined,
    preset_config:
      preset === 'dockerfile' && app.dockerfilePath
        ? { dockerfilePath: app.dockerfilePath }
        : preset === 'docker-compose'
          ? { composePath: app.composeFiles?.[0] ?? 'docker-compose.yml' }
          : undefined,
  }
}

export type ImportRowState =
  | { state: 'running' }
  | { state: 'done' }
  | { state: 'failed'; message: string }

/**
 * Where each imported app stands, from the server's steps. `services[i]` is
 * the app sent at position `i`. A row is done once its service is in the
 * project and follows the repository; its first deploy is reported on its
 * own row too, because a service that never deploys is not done either.
 */
export function importRowStates(
  instantiation: Pick<
    InstantiationResponse,
    'services' | 'steps' | 'status' | 'error'
  >
): ImportRowState[] {
  const finished =
    instantiation.status !== 'pending' && instantiation.status !== 'running'
  return instantiation.services.map((service) => {
    const own = instantiation.steps.filter((step) =>
      step.key.endsWith(`:${service.template_service}`)
    )
    const failed = own.find((step) => step.status === 'failed')
    if (failed) {
      return {
        state: 'failed',
        message: failed.error ?? `${failed.label} failed`,
      }
    }
    if (own.length > 0 && own.every((step) => step.status === 'done')) {
      return { state: 'done' }
    }
    if (finished && instantiation.status !== 'succeeded') {
      return {
        state: 'failed',
        message:
          instantiation.status === 'interrupted'
            ? 'The server restarted before this finished. Retry to continue.'
            : `Not added yet: ${instantiation.error ?? 'an earlier step failed'}`,
      }
    }
    return { state: 'running' }
  })
}

/** Most top-level apps a scan preselects; past this, the user picks. */
export const PRESELECT_LIMIT = 3

/** Whether `dir` sits inside another detected app (a test, fixture or tool). */
export function isNestedApp(dir: string, allDirs: readonly string[]): boolean {
  return allDirs.some(
    (other) => other !== dir && (other === '.' || dir.startsWith(`${other}/`))
  )
}

/**
 * The app directories a scan starts with checked: top-level apps that have
 * no service yet, but only when there are few of them. A large monorepo
 * (tools, docs, fixtures) starts with nothing checked, so importing never
 * creates a pile of services by default.
 */
export function defaultSelection(
  allDirs: readonly string[],
  deployed: ReadonlySet<string>
): Set<string> {
  const candidates = allDirs.filter(
    (d) => !deployed.has(d) && !isNestedApp(d, allDirs)
  )
  const topLevel = allDirs.filter((d) => !isNestedApp(d, allDirs))
  return new Set(topLevel.length <= PRESELECT_LIMIT ? candidates : [])
}

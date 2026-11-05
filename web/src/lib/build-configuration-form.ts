// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectPresetResponse, ProjectResponse } from '@/api/client'
import {
  canonicalProjectDirectory,
  isRepositoryRootDirectory,
} from '@/lib/project-directory'
import {
  isSameDockerfile,
  normalizePresetPath,
  splitPresetSelection,
} from '@/lib/preset-selection'
import { z } from 'zod'

export const DEFAULT_DOCKERFILE_PATH = 'Dockerfile'
export const DEFAULT_COMPOSE_PATH = 'docker-compose.yml'
const DEFAULT_DIRECTORY = './'

export const buildConfigurationSchema = z.object({
  preset: z.string(),
  directory: z.string(),
  pullOnlyRootDirectory: z.boolean(),
  dockerfilePath: z.string(),
  buildContext: z.string(),
  composePath: z.string(),
})

export type BuildConfigurationValues = z.infer<typeof buildConfigurationSchema>

/** The fields of the settings request this form can change. */
export interface BuildConfigurationPatch {
  preset?: string
  directory?: string
  pull_only_root_directory?: boolean
  preset_config?: Record<string, unknown>
}

export function isDockerfilePreset(preset: string): boolean {
  return preset === 'dockerfile'
}

export function isComposePreset(preset: string): boolean {
  return preset === 'docker-compose' || preset === 'dockercompose'
}

function presetConfigOf(project: ProjectResponse): Record<string, unknown> {
  return (project.preset_config ?? {}) as Record<string, unknown>
}

function textOr(value: unknown, fallback: string): string {
  return typeof value === 'string' && value ? value : fallback
}

/** The form's values for what is saved on the project. */
export function buildConfigurationValues(
  project: ProjectResponse
): BuildConfigurationValues {
  const config = presetConfigOf(project)
  return {
    preset: (project.preset ?? '').toString(),
    directory: project.directory || DEFAULT_DIRECTORY,
    pullOnlyRootDirectory: !!project.pull_only_root_directory,
    dockerfilePath: textOr(config.dockerfilePath, DEFAULT_DOCKERFILE_PATH),
    buildContext: textOr(config.buildContext, ''),
    composePath: textOr(config.composePath, DEFAULT_COMPOSE_PATH),
  }
}

/**
 * The values as they will be stored: trimmed, blanks replaced by their
 * defaults, and pull-only off at the repository root where it means nothing.
 */
export function normalizeBuildConfiguration(
  values: BuildConfigurationValues
): BuildConfigurationValues {
  const directory = canonicalProjectDirectory(values.directory)
  return {
    preset: values.preset,
    directory,
    pullOnlyRootDirectory:
      values.pullOnlyRootDirectory && !isRepositoryRootDirectory(directory),
    dockerfilePath: values.dockerfilePath.trim() || DEFAULT_DOCKERFILE_PATH,
    buildContext: values.buildContext.trim(),
    composePath: values.composePath.trim() || DEFAULT_COMPOSE_PATH,
  }
}

/**
 * What differs from the saved project. Empty means there is nothing to save, so
 * a stray space or retyping the saved value does not offer a save.
 *
 * `preset` is only sent when it changed because selecting the base Nixpacks
 * preset resets forced providers (`presetConfigForSelection`).
 */
export function buildConfigurationPatch(
  project: ProjectResponse,
  values: BuildConfigurationValues
): BuildConfigurationPatch {
  const saved = buildConfigurationValues(project)
  const next = normalizeBuildConfiguration(values)
  const config = presetConfigOf(project)
  const patch: BuildConfigurationPatch = {}

  if (next.preset !== saved.preset) patch.preset = next.preset
  if (next.directory !== canonicalProjectDirectory(saved.directory)) {
    patch.directory = next.directory
  }
  if (next.pullOnlyRootDirectory !== saved.pullOnlyRootDirectory) {
    patch.pull_only_root_directory = next.pullOnlyRootDirectory
  }

  if (
    isDockerfilePreset(next.preset) &&
    (next.dockerfilePath !== saved.dockerfilePath ||
      next.buildContext !== saved.buildContext)
  ) {
    patch.preset_config = {
      ...config,
      preset: 'dockerfile',
      dockerfilePath: next.dockerfilePath,
      // Explicit null: the server keeps any key a patch omits, so leaving it
      // out would never clear the override.
      buildContext: next.buildContext || null,
    }
  }

  if (isComposePreset(next.preset) && next.composePath !== saved.composePath) {
    patch.preset_config = {
      ...config,
      preset: 'docker-compose',
      composePath: next.composePath,
    }
  }

  return patch
}

/**
 * The config to send for a save. The server refuses any request that still
 * carries legacy per-service sandbox grants, whichever preset it is for, so a
 * Compose project holding them could not otherwise move to another framework.
 * Compose always sends the empty list because the server keeps an omitted key.
 */
export function withoutLegacySandboxGrants<T extends Record<string, unknown>>(
  preset: string | null | undefined,
  config: T
): T {
  const grants = config.unsandboxedServices
  const hasGrants = Array.isArray(grants) && grants.length > 0
  return isComposePreset(preset ?? '') || hasGrants
    ? { ...config, unsandboxedServices: [] }
    : config
}

/**
 * The Compose file a detected card brings. Detection lists paths from the
 * repository root, but a project stores them relative to its root directory,
 * and a card lists every Compose file in the repository, not only its own.
 */
function detectedComposePath(
  card: ProjectPresetResponse | undefined,
  directory: string
): string {
  const files = card?.composeFiles ?? []
  const path = normalizePresetPath(directory)
  if (path === 'root') return files[0] || DEFAULT_COMPOSE_PATH
  const prefix = `${path.replace(/\/+$/, '')}/`
  const file = files.find((item) => item.startsWith(prefix))
  return file ? file.slice(prefix.length) : DEFAULT_COMPOSE_PATH
}

/** A repository path with no `./`, leading or trailing slash; the root is ''. */
function repoPath(path: string): string {
  return path
    .trim()
    .replace(/\\/g, '/')
    .replace(/^(\.\/|\/)+/, '')
    .replace(/\/+$/, '')
    .replace(/^\.$/, '')
}

/**
 * Where `directory` sits inside the build context: '' when they are the same
 * (or no context is set, so the context is the directory), the path between
 * them when the context contains it, and `undefined` when it does not.
 */
function directoryWithinContext(
  directory: string,
  buildContext: string
): string | undefined {
  if (!buildContext.trim()) return ''
  const context = repoPath(buildContext)
  const dir = repoPath(directory)
  if (context === dir) return ''
  if (context === '') return dir
  if (dir.startsWith(`${context}/`)) return dir.slice(context.length + 1)
  return undefined
}

/**
 * The Dockerfile path as detection names it, relative to the root directory.
 * The stored path is relative to the build context, so with a context above
 * the root directory it carries the path between them, which this removes.
 * Used to tell which detected card is the current file.
 */
export function dockerfileWithinDirectory(
  values: Pick<
    BuildConfigurationValues,
    'directory' | 'buildContext' | 'dockerfilePath'
  >
): string {
  const prefix = directoryWithinContext(values.directory, values.buildContext)
  const path = repoPath(values.dockerfilePath)
  if (prefix && path.startsWith(`${prefix}/`)) {
    return path.slice(prefix.length + 1)
  }
  return values.dockerfilePath
}

/**
 * The values after choosing a card in `FrameworkSelector`.
 *
 * A detected card (`slug::path`) is the framework found at that path, so it
 * brings its directory and the file detection found there. Several Dockerfiles
 * can share a path, so the file comes from the card itself, not the key. A card
 * from the full catalog (a bare slug) says nothing about where the code is, so
 * the root directory and file paths stay as they are: changing the framework of
 * a monorepo service must not move it to the repository root.
 *
 * Choosing what is already selected changes nothing, so re-clicking the current
 * card cannot replace a path someone set by hand.
 */
export function applyFrameworkSelection(
  current: BuildConfigurationValues,
  selection: string,
  card?: ProjectPresetResponse
): BuildConfigurationValues {
  const { preset, directory } = splitPresetSelection(selection)
  if (!selection.includes('::')) return { ...current, preset }

  const samePlace =
    preset === current.preset &&
    normalizePresetPath(directory) === normalizePresetPath(current.directory)
  const dockerfile = isDockerfilePreset(preset)
  const detectedFile = card?.dockerfilePath || DEFAULT_DOCKERFILE_PATH
  if (
    samePlace &&
    (!dockerfile ||
      isSameDockerfile(detectedFile, dockerfileWithinDirectory(current)))
  ) {
    return current
  }
  // Detection names the file from its own directory, but the build reads the
  // Dockerfile path from the build context. A context that contains the
  // directory is kept, with the path rewritten to start from it; any other
  // context would point the build at the wrong file, so it is cleared.
  const within = dockerfile
    ? directoryWithinContext(directory, current.buildContext)
    : undefined
  const dockerfilePath = !dockerfile
    ? current.dockerfilePath
    : within
      ? `${within}/${repoPath(detectedFile)}`
      : detectedFile
  return {
    ...current,
    preset,
    directory,
    dockerfilePath,
    buildContext: within !== undefined ? current.buildContext : '',
    composePath: isComposePreset(preset)
      ? detectedComposePath(card, directory)
      : current.composePath,
  }
}

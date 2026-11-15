// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PresetInfo, ProjectPresetResponse } from '@/api/client'

/**
 * Helpers for the composite preset-selection key used by `FrameworkSelector`.
 *
 * A repository can contain the same preset at several paths (a monorepo with
 * five Dockerfiles), so a bare slug like `dockerfile` is ambiguous. The
 * selector therefore identifies a choice as `"<slug>::<path>"` where `path`
 * is `root` for the repository root.
 *
 * That composite key is a **UI concern only** — the API takes `preset` (the
 * slug) and `directory` separately. Submitting the composite verbatim is
 * rejected with "Unknown preset: dockerfile::examples/go-error-tracking", and
 * storing a bare slug makes every card sharing that slug render as selected.
 * Both bugs came from doing this by hand in one place and not the other, so
 * the conversion lives here.
 */

/** Path segment used for the repository root in a selection key. */
const ROOT = 'root'

/**
 * Normalize any of the directory/path shapes in play (`'./'`, `'.'`, `''`,
 * `'./sub'`, `'sub'`) to the key's path form, so a configured directory and a
 * detected preset's path can be compared directly.
 */
export function normalizePresetPath(value: string | null | undefined): string {
  const v = (value ?? '').trim()
  if (!v || v === '.' || v === './') return ROOT
  return (v.startsWith('./') ? v.slice(2) : v) || ROOT
}

/**
 * Build the selector key for a preset at a directory.
 *
 * `directory` accepts the API's shapes (`'./'`, `'.'`, `'./sub'`, `'sub'`, or
 * empty) and normalizes to the key's path form.
 */
export function presetSelectionKey(
  preset: string | null | undefined,
  directory: string | null | undefined
): string {
  if (!preset) return ''
  return `${preset}::${normalizePresetPath(directory)}`
}

/**
 * Split a selector key back into the API's `preset` + `directory` pair.
 *
 * Tolerates a bare slug (no `::`), treating it as the repository root, and
 * passes `custom` through with a root directory — matching how the git
 * settings view has always handled the "custom" choice.
 */
export function splitPresetSelection(value: string): {
  preset: string
  directory: string
} {
  if (value === 'custom') return { preset: 'custom', directory: './' }
  const [preset, path] = value.split('::')
  if (!path || path === ROOT || path === '.' || path.trim() === '') {
    return { preset, directory: './' }
  }
  const stripped = path.startsWith('./') ? path.slice(2) : path
  return { preset, directory: `./${stripped}` }
}

/**
 * Keep inline Nixpacks settings when the base catalog entry is selected, while
 * making that selection explicitly reset provider forcing to auto-detection.
 */
export function presetConfigForSelection(
  preset: string,
  currentConfig: unknown
): unknown {
  if (preset !== 'nixpacks') return currentConfig

  if (
    currentConfig &&
    typeof currentConfig === 'object' &&
    !Array.isArray(currentConfig) &&
    (currentConfig as Record<string, unknown>).preset === 'nixpacks'
  ) {
    return {
      ...(currentConfig as Record<string, unknown>),
      providers: [],
    }
  }

  return { providers: [] }
}

const DEFAULT_DOCKERFILE = 'Dockerfile'

/** Whether two Dockerfile paths name one file; blank means the default file. */
export function isSameDockerfile(
  a: string | null | undefined,
  b: string | null | undefined
): boolean {
  const normalize = (value: string | null | undefined) =>
    (value ?? '').trim().replace(/^\.\//, '') || DEFAULT_DOCKERFILE
  return normalize(a) === normalize(b)
}

/**
 * The file that tells apart the Dockerfile cards of a selection. Any other
 * framework's card names no file, so a path left over from an earlier choice
 * must not stop that card reading as selected.
 */
export function selectedDockerfileFor(
  selection: string,
  dockerfilePath: string | undefined
): string | undefined {
  return splitPresetSelection(selection).preset === 'dockerfile'
    ? dockerfilePath
    : undefined
}

/**
 * The public detection endpoint's snake_case presets in the camelCase shape
 * `FrameworkSelector` reads. Dropping `dockerfile_path` here makes a card for
 * `docker/Dockerfile` build the root `Dockerfile` instead.
 */
export function detectedPresetsFromPublic(
  presets: readonly PresetInfo[]
): ProjectPresetResponse[] {
  return presets.map((preset) => ({
    preset: preset.preset,
    presetLabel: preset.preset_label,
    exposedPort: preset.exposed_port,
    iconUrl: preset.icon_url,
    projectType: preset.project_type,
    path: preset.path,
    composeFiles: preset.compose_files,
    dockerfilePath: preset.dockerfile_path,
  }))
}

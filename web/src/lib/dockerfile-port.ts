// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

type DetectedPresetWithPort = {
  preset: string
  path?: string | null
  exposedPort?: number | null
  exposed_port?: number | null
}

function normalizePath(path: string | undefined | null): string {
  if (!path || path === '.' || path === './' || path === 'root') return 'root'
  return path.replace(/^\.\//, '').replace(/\/$/, '')
}

/** Resolve the detected port for a `preset::path` selector value. */
export function detectedPortForSelection(
  presets: DetectedPresetWithPort[] | undefined,
  selection: string | undefined
): number | undefined {
  if (!presets?.length || !selection) return undefined

  const [slug, selectedPath] = selection.split('::')
  const candidates = presets.filter((preset) => preset.preset === slug)
  const match = selectedPath
    ? candidates.find(
        (preset) => normalizePath(preset.path) === normalizePath(selectedPath)
      )
    : candidates[0]
  const port = match?.exposedPort ?? match?.exposed_port

  return typeof port === 'number' && port > 0 && port <= 65535
    ? port
    : undefined
}

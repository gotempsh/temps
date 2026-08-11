import type { ProjectPresetResponse } from '@/api/client'

/** Keep one representative per detected framework while preserving API order. */
export function uniqueRepositoryPresets(
  presets: ProjectPresetResponse[] | null | undefined
): ProjectPresetResponse[] {
  if (!presets?.length) return []

  const seen = new Set<string>()
  return presets.filter((preset) => {
    if (seen.has(preset.preset)) return false
    seen.add(preset.preset)
    return true
  })
}

/**
 * Prefer an explicit live-scan result, including an empty result, while using
 * the repository's persisted detections before a live scan has completed.
 */
export function repositoryPresetsForDisplay(
  livePresets: ProjectPresetResponse[] | null | undefined,
  cachedPresets: ProjectPresetResponse[] | null | undefined
): ProjectPresetResponse[] {
  return uniqueRepositoryPresets(livePresets ?? cachedPresets)
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RepositoryResponse } from '@/api/client'
import { getRepositoryPresetLiveOptions } from '@/api/client/@tanstack/react-query.gen'
import { repositoryPresetsForDisplay } from '@/lib/repository-presets'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, Loader2 } from 'lucide-react'

interface RepositoryPresetLogosProps {
  repository: RepositoryResponse
  automaticDetectionEnabled: boolean
}

export function RepositoryPresetLogos({
  repository,
  automaticDetectionEnabled,
}: RepositoryPresetLogosProps) {
  const presetQuery = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repository.id },
    }),
    enabled: automaticDetectionEnabled,
    retry: false,
    staleTime: 5 * 60 * 1000,
  })
  // Repository sync and previous live scans persist detections on the row.
  // Render that cache immediately; only replace it after this repository has
  // been explicitly refreshed. Automatically scanning every visible row
  // would turn one list view into dozens of provider tree requests.
  const presets = repositoryPresetsForDisplay(
    presetQuery.data?.presets,
    repository.preset
  )
  const hasDetectedPresets = presets.length > 0

  return (
    <div
      className="flex h-8 shrink-0 items-center gap-1 px-1.5"
      aria-label={`Detected presets for ${repository.full_name}`}
    >
      {presetQuery.isFetching && !hasDetectedPresets ? (
        <Loader2 className="size-4 shrink-0 animate-spin stroke-muted-foreground" />
      ) : presetQuery.isError && !hasDetectedPresets ? (
        <AlertCircle className="size-4 shrink-0 stroke-destructive" />
      ) : hasDetectedPresets ? (
        <>
          {presets.slice(0, 2).map((preset) => (
            <img
              key={preset.preset}
              src={preset.iconUrl || '/presets/custom.svg'}
              alt=""
              className="size-4 shrink-0 object-contain"
              title={preset.presetLabel}
              onError={(event) => {
                event.currentTarget.src = '/presets/custom.svg'
              }}
            />
          ))}
          {presets.length > 2 && (
            <span className="tabular-nums text-muted-foreground">
              +{presets.length - 2}
            </span>
          )}
        </>
      ) : presetQuery.data ? (
        <span className="text-muted-foreground" title="No presets detected">
          —
        </span>
      ) : automaticDetectionEnabled ? (
        <Loader2 className="size-4 shrink-0 animate-spin stroke-muted-foreground" />
      ) : null}
    </div>
  )
}

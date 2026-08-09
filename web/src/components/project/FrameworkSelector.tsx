import { useState, useMemo } from 'react'
import { Folder, AlertCircle, Check, Grid3x3, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import {
  GitConnectionExpiredAlert,
  isGitAuthError,
} from '@/components/git-providers/GitConnectionExpiredAlert'
import type { ProjectPresetResponse, PresetResponse } from '@/api/client'
import { usePresets } from '@/contexts/PresetContext'

// Helper function to normalize path for consistent comparison
// Normalizes '.', './', and empty strings to 'root'
function normalizePath(path: string | undefined | null): string {
  if (!path || path === '.' || path === './') {
    return 'root'
  }
  return path
}

/** Flexible type that accepts either full RepositoryPresetResponse or just { presets } */
type PresetDataType = { presets: ProjectPresetResponse[] } | undefined

interface FrameworkSelectorProps {
  presetData: PresetDataType
  isLoading: boolean
  /** True while a refetch (e.g. triggered by the Refresh button) is in flight
   * on top of already-loaded data. Distinct from `isLoading`, which React
   * Query only sets true for the *initial* fetch — without this, clicking
   * Refresh after data has loaded once gives no visual feedback at all. */
  isRefreshing?: boolean
  error?: Error | null
  selectedPreset: string
  onSelectPreset: (value: string) => void
  onRefresh?: () => void
  disabled?: boolean
}

export function FrameworkSelector({
  presetData,
  isLoading,
  isRefreshing = false,
  error,
  selectedPreset,
  onSelectPreset,
  onRefresh,
  disabled = false,
}: FrameworkSelectorProps) {
  const [manualMode, setManualMode] = useState(false)
  const {
    presets: availablePresets,
    getPresetBySlug,
    isLoading: presetsLoading,
  } = usePresets()

  const rawDetectedProjects = useMemo(
    () => presetData?.presets || [],
    [presetData?.presets]
  )

  // If the currently selected preset+path isn't in the detected list, inject it
  // so the user sees their current selection highlighted among detected presets
  const detectedProjects = useMemo(() => {
    if (
      !selectedPreset ||
      selectedPreset === 'custom' ||
      rawDetectedProjects.length === 0
    ) {
      return rawDetectedProjects
    }
    const [selectedSlug, selectedPath] = selectedPreset.split('::')
    if (!selectedSlug || !selectedPath) return rawDetectedProjects

    const normalizedSelectedPath = normalizePath(selectedPath)
    const alreadyExists = rawDetectedProjects.some((p) => {
      return (
        p.preset === selectedSlug &&
        normalizePath(p.path) === normalizedSelectedPath
      )
    })

    if (alreadyExists) return rawDetectedProjects

    // Inject the current project's preset at the beginning
    const presetInfo = getPresetBySlug(selectedSlug)
    const injected: ProjectPresetResponse = {
      preset: selectedSlug,
      presetLabel: presetInfo?.label || selectedSlug,
      exposedPort: presetInfo?.default_port || 0,
      iconUrl: presetInfo?.icon_url || '',
      projectType: presetInfo?.project_type || 'server',
      path: selectedPath === 'root' ? './' : selectedPath,
    }
    return [injected, ...rawDetectedProjects]
  }, [rawDetectedProjects, selectedPreset, getPresetBySlug])

  const hasDetectedPresets = detectedProjects.length > 0 && !error

  // Simple rule: if we have detected presets, show them. Otherwise show all.
  // Only exception: manual mode (user clicked "Browse all presets")
  const shouldShowAllPresets = manualMode || (!hasDetectedPresets && !isLoading)

  // Get presets to display based on mode
  const presetsToDisplay = useMemo(() => {
    if (shouldShowAllPresets) {
      // Show all available presets (excluding "custom" which is shown separately)
      return availablePresets.filter((preset) => preset.slug !== 'custom')
    }
    // Show all detected presets (including injected current selection if needed)
    return detectedProjects
  }, [shouldShowAllPresets, detectedProjects, availablePresets])

  if (isLoading || presetsLoading) {
    return (
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <Skeleton className="h-5 w-32" />
          <Skeleton className="h-8 w-24" />
        </div>
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {[1, 2, 3].map((i) => (
            <Card key={i}>
              <CardContent className="p-4">
                <div className="flex items-start gap-3">
                  <Skeleton className="w-12 h-12 rounded" />
                  <div className="flex-1 space-y-2">
                    <Skeleton className="h-4 w-24" />
                    <Skeleton className="h-3 w-full" />
                    <div className="flex items-center gap-1 mt-2">
                      <Skeleton className="h-3 w-3 rounded-full" />
                      <Skeleton className="h-3 w-16" />
                    </div>
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <label className="text-sm font-medium">Framework Preset</label>
        <div className="flex items-center gap-2">
          {/* Refresh button */}
          {onRefresh && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onRefresh}
              disabled={isLoading || isRefreshing}
              className="text-xs"
            >
              <RefreshCw
                className={`h-3 w-3 mr-1 ${isLoading || isRefreshing ? 'animate-spin' : ''}`}
              />
              Refresh
            </Button>
          )}

          {/* Toggle between detected and all presets */}
          {!shouldShowAllPresets && !manualMode && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setManualMode(true)}
              className="text-xs"
            >
              <Grid3x3 className="h-3 w-3 mr-1" />
              Browse all presets
            </Button>
          )}

          {manualMode && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setManualMode(false)}
              className="text-xs"
            >
              Back to detected
            </Button>
          )}
        </div>
      </div>

      {/* Show error/info alerts — must match what's actually displayed below */}
      {/* An expired git credential is not a detection failure: telling the
          user to "pick one manually" hides an auth problem that will break
          the next step too, so it gets its own actionable state. */}
      {error && isGitAuthError(error) && (
        <GitConnectionExpiredAlert operation="detect this project's framework" />
      )}
      {error && !isGitAuthError(error) && shouldShowAllPresets && (
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>
            Could not detect presets automatically. Please select one manually
            from the list below.
          </AlertDescription>
        </Alert>
      )}

      {shouldShowAllPresets && !error && !manualMode && (
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>
            Select a preset for your project from the list below.
          </AlertDescription>
        </Alert>
      )}

      {!shouldShowAllPresets && !manualMode && (
        <Alert>
          <AlertDescription>
            ✓ We detected the following preset
            {detectedProjects.length > 1 ? 's' : ''} in your repository. You can
            browse all presets if you prefer.
          </AlertDescription>
        </Alert>
      )}

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {shouldShowAllPresets ? (
          // Show all available presets
          <>
            {(presetsToDisplay as PresetResponse[]).map((preset) => {
              // Normalize comparison: check both slug and slug::root formats
              const isSelected =
                selectedPreset === preset.slug ||
                selectedPreset === `${preset.slug}::root` ||
                selectedPreset.startsWith(`${preset.slug}::`)

              return (
                <PresetCard
                  key={preset.slug}
                  preset={preset}
                  isSelected={isSelected}
                  onSelect={() => onSelectPreset(preset.slug)}
                  disabled={disabled}
                />
              )
            })}
          </>
        ) : (
          // Show detected presets
          <>
            {(presetsToDisplay as ProjectPresetResponse[]).map((project) => (
              <DetectedPresetCard
                key={`${project.preset}::${project.path || 'root'}`}
                project={project}
                selectedPreset={selectedPreset}
                onSelectPreset={onSelectPreset}
                disabled={disabled}
                getPresetBySlug={getPresetBySlug}
              />
            ))}
          </>
        )}
      </div>
    </div>
  )
}

/**
 * The `server` / `static` / `container` chip shown beside a preset name.
 *
 * Shared by both card types so the two grids can never drift apart, and so the
 * ui.sh picker toggles them together while a treatment is being chosen.
 */
function TypeBadge({ children }: { children: React.ReactNode }) {
  // Outline keeps the chip visible without adding a filled surface. The old
  // `secondary` was #fafafa on a white card — an invisible pill that read as
  // bare near-black text. Matches the repository-list preset badges.
  return (
    <Badge variant="outline" className="shrink-0 text-xs font-normal">
      {children}
    </Badge>
  )
}

// Component for showing a preset from the full catalog
function PresetCard({
  preset,
  isSelected,
  onSelect,
  disabled,
}: {
  preset: PresetResponse
  isSelected: boolean
  onSelect: () => void
  disabled: boolean
}) {
  return (
    <Card
      className={`relative cursor-pointer transition-shadow hover:shadow-sm dark:hover:shadow-none ${
        isSelected
          ? 'ring-2 ring-primary ring-offset-1 ring-offset-background'
          : ''
      } ${disabled ? 'cursor-not-allowed opacity-50' : ''}`}
      onClick={() => !disabled && onSelect()}
      aria-pressed={isSelected}
    >
      <CardContent className="p-4">
        {isSelected && (
          <span className="absolute right-3 top-3 flex size-5 items-center justify-center rounded-full bg-primary text-primary-foreground">
            <Check className="size-3" />
          </span>
        )}
        <div className="flex items-start gap-3">
          {/* Tile keeps light brand marks legible on a white card and removes
              the need for `dark:invert`, which distorted coloured logos. */}
          <div className="flex size-11 shrink-0 items-center justify-center rounded-lg bg-muted outline-1 -outline-offset-1 outline-black/5 dark:bg-white/5 dark:outline-white/10">
            <img
              src={preset.icon_url || '/presets/custom.svg'}
              alt=""
              className="size-6 object-contain"
              onError={(e) => {
                e.currentTarget.src = '/presets/custom.svg'
              }}
            />
          </div>

          <div className="min-w-0 flex-1 pr-6">
            <div className="mb-1 flex items-center gap-2">
              <h3 className="truncate text-sm font-semibold">{preset.label}</h3>
              <TypeBadge>{preset.project_type}</TypeBadge>
            </div>

            <p className="line-clamp-2 text-xs text-muted-foreground">
              {preset.description}
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

// Component for showing a detected preset from the repository
function DetectedPresetCard({
  project,
  selectedPreset,
  onSelectPreset,
  disabled,
  getPresetBySlug,
}: {
  project: ProjectPresetResponse
  selectedPreset: string
  onSelectPreset: (value: string) => void
  disabled: boolean
  getPresetBySlug: (slug: string) => PresetResponse | undefined
}) {
  // Normalize the path for consistent comparison
  const normalizedPath = normalizePath(project.path)
  const presetKey = `${project.preset}::${normalizedPath}`

  // Check if this preset is selected by comparing normalized paths
  const isSelected = useMemo(() => {
    const [selectedSlug, selectedPath] = selectedPreset.split('::')
    // A selection without a path means "this preset at the repository root".
    // Matching on the slug alone lit up every card sharing it, so a monorepo
    // with five Dockerfiles showed five "Selected" cards at once.
    if (!selectedPath) {
      return project.preset === selectedSlug && normalizedPath === 'root'
    }
    const normalizedSelectedPath = normalizePath(selectedPath)
    return (
      project.preset === selectedSlug &&
      normalizedPath === normalizedSelectedPath
    )
  }, [selectedPreset, project.preset, normalizedPath])

  const presetInfo = getPresetBySlug(project.preset)
  const fallbackPreset = getPresetBySlug('nixpacks')
  const iconSrc =
    presetInfo?.icon_url || fallbackPreset?.icon_url || '/presets/custom.svg'
  const label = project.presetLabel || presetInfo?.label || project.preset
  const projectType = presetInfo?.project_type || 'Server'
  const description = presetInfo?.description || 'Custom configuration'
  const pathLabel = project.path && project.path !== '.' ? project.path : './'
  const onPick = () => !disabled && onSelectPreset(presetKey)

  return (
    <Card
      className={`relative cursor-pointer transition-shadow hover:shadow-sm dark:hover:shadow-none ${
        isSelected
          ? 'ring-2 ring-primary ring-offset-1 ring-offset-background'
          : ''
      } ${disabled ? 'cursor-not-allowed opacity-50' : ''}`}
      onClick={onPick}
      aria-pressed={isSelected}
    >
      <CardContent className="p-4">
        {isSelected && (
          <span className="absolute right-3 top-3 flex size-5 items-center justify-center rounded-full bg-primary text-primary-foreground">
            <Check className="size-3" />
          </span>
        )}
        <div className="flex items-start gap-3">
          <div className="flex size-11 shrink-0 items-center justify-center rounded-lg bg-muted outline-1 -outline-offset-1 outline-black/5 dark:bg-white/5 dark:outline-white/10">
            <img
              src={iconSrc}
              alt=""
              className="size-6 object-contain"
              onError={(e) => {
                e.currentTarget.src = '/presets/custom.svg'
              }}
            />
          </div>
          <div className="min-w-0 flex-1 pr-6">
            <div className="mb-1 flex items-center gap-2">
              <h3 className="truncate text-sm font-semibold">{label}</h3>
              <TypeBadge>{projectType}</TypeBadge>
            </div>
            <p className="line-clamp-2 text-xs text-muted-foreground">
              {description}
            </p>
            <div className="mt-2 flex items-center gap-1.5 text-xs text-muted-foreground">
              <Folder className="size-3 shrink-0" />
              <span className="truncate font-mono" title={pathLabel}>
                {pathLabel}
              </span>
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

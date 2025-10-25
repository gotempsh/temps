import { useState, useMemo } from 'react'
import { Folder, AlertCircle, Grid3x3, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import type {
  RepositoryPresetResponse,
  ProjectPresetResponse,
  PresetResponse,
} from '@/api/client'
import { usePresets } from '@/contexts/PresetContext'

// Helper function to normalize path for consistent comparison
// Normalizes '.', './', and empty strings to 'root'
function normalizePath(path: string | undefined | null): string {
  if (!path || path === '.' || path === './') {
    return 'root'
  }
  return path
}

interface FrameworkSelectorProps {
  presetData: RepositoryPresetResponse | undefined
  isLoading: boolean
  error?: Error | null
  selectedPreset: string
  onSelectPreset: (value: string) => void
  onRefresh?: () => void
  disabled?: boolean
}

export function FrameworkSelector({
  presetData,
  isLoading,
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

  const detectedProjects = useMemo(
    () => presetData?.presets || [],
    [presetData?.presets]
  )
  const hasDetectedPresets = detectedProjects.length > 0 && !error

  // Check if the selected preset is in the detected list
  const isSelectedInDetected = useMemo(() => {
    if (!selectedPreset || selectedPreset === 'custom') return true
    // Normalize the selected preset for comparison
    const [selectedSlug, selectedPath] = selectedPreset.split('::')
    const normalizedSelectedPath = normalizePath(selectedPath)

    // Check if the full preset key (preset::normalizedPath) is in detected list
    return detectedProjects.some((p) => {
      const normalizedProjectPath = normalizePath(p.path)
      return (
        p.preset === selectedSlug &&
        normalizedProjectPath === normalizedSelectedPath
      )
    })
  }, [selectedPreset, detectedProjects])

  // Determine if we should show all available presets or detected ones
  // Force show all if selected preset is not in detected list
  const shouldShowAllPresets =
    !hasDetectedPresets || manualMode || !isSelectedInDetected

  // Get presets to display based on mode
  const presetsToDisplay = useMemo(() => {
    if (shouldShowAllPresets) {
      // Show all available presets (excluding "custom" which is shown separately)
      return availablePresets.filter((preset) => preset.slug !== 'custom')
    }
    // Show all detected presets (no need to collapse when multiple are found)
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
              disabled={isLoading}
              className="text-xs"
            >
              <RefreshCw className={`h-3 w-3 mr-1 ${isLoading ? 'animate-spin' : ''}`} />
              Refresh
            </Button>
          )}

          {/* Toggle between detected and all presets */}
          {hasDetectedPresets && !manualMode && (
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

      {/* Show error/info alerts */}
      {error && !manualMode && (
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>
            Could not detect presets automatically. Please select one manually
            from the list below.
          </AlertDescription>
        </Alert>
      )}

      {!hasDetectedPresets && !error && !manualMode && (
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertDescription>
            No presets detected in this repository. Please select one from the
            list below.
          </AlertDescription>
        </Alert>
      )}

      {hasDetectedPresets && !manualMode && (
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
      className={`cursor-pointer transition-all hover:border-primary/50 ${
        isSelected ? 'border-primary border-2 bg-primary/5' : 'border-border'
      } ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      onClick={() => !disabled && onSelect()}
    >
      <CardContent className="p-4">
        <div className="flex items-start gap-3">
          <div className="flex-shrink-0">
            <img
              src={preset.icon_url || '/presets/custom.svg'}
              alt={preset.label}
              className="w-12 h-12 object-contain dark:invert"
              onError={(e) => {
                e.currentTarget.src = '/presets/custom.svg'
              }}
            />
          </div>

          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <h3 className="font-semibold text-sm truncate">{preset.label}</h3>
              <Badge variant="secondary" className="text-xs flex-shrink-0">
                {preset.project_type}
              </Badge>
            </div>

            <p className="text-xs text-muted-foreground line-clamp-2">
              {preset.description}
            </p>
          </div>
        </div>

        {isSelected && (
          <div className="mt-3 pt-3 border-t border-border">
            <div className="flex items-center gap-1.5 text-xs font-medium text-primary">
              <div className="w-1.5 h-1.5 bg-primary rounded-full" />
              Selected
            </div>
          </div>
        )}
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
    const normalizedSelectedPath = normalizePath(selectedPath)
    return (
      project.preset === selectedSlug &&
      normalizedPath === normalizedSelectedPath
    )
  }, [selectedPreset, project.preset, normalizedPath])

  const presetInfo = getPresetBySlug(project.preset)
  const fallbackPreset = getPresetBySlug('nixpacks')

  return (
    <Card
      className={`cursor-pointer transition-all hover:border-primary/50 ${
        isSelected ? 'border-primary border-2 bg-primary/5' : 'border-border'
      } ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      onClick={() => !disabled && onSelectPreset(presetKey)}
    >
      <CardContent className="p-4">
        <div className="flex items-start gap-3">
          {/* Preset Icon */}
          <div className="flex-shrink-0">
            <img
              src={
                presetInfo?.icon_url ||
                fallbackPreset?.icon_url ||
                '/presets/custom.svg'
              }
              alt={presetInfo?.label || project.preset}
              className="w-12 h-12 object-contain dark:invert"
              onError={(e) => {
                e.currentTarget.src = '/presets/custom.svg'
              }}
            />
          </div>

          {/* Preset Info */}
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <h3 className="font-semibold text-sm truncate">
                {project.preset_label || presetInfo?.label || project.preset}
              </h3>
              <Badge variant="secondary" className="text-xs flex-shrink-0">
                {presetInfo?.project_type || 'Server'}
              </Badge>
            </div>

            <p className="text-xs text-muted-foreground line-clamp-2 mb-2">
              {presetInfo?.description || 'Custom configuration'}
            </p>

            {/* Path indicator for monorepo */}
            {project.path && project.path !== '.' && (
              <div className="flex items-start gap-1 text-xs text-muted-foreground mt-2 p-1.5 bg-muted/50 rounded">
                <Folder className="h-3 w-3 flex-shrink-0 mt-0.5" />
                <span className="font-mono break-all" title={project.path}>
                  {project.path}
                </span>
              </div>
            )}

            {/* Root indicator */}
            {(!project.path || project.path === '.') && (
              <div className="text-xs text-muted-foreground mt-2">
                <span className="font-mono">./</span>
              </div>
            )}
          </div>
        </div>

        {/* Selected indicator */}
        {isSelected && (
          <div className="mt-3 pt-3 border-t border-border">
            <div className="flex items-center gap-1.5 text-xs font-medium text-primary">
              <div className="w-1.5 h-1.5 bg-primary rounded-full" />
              Selected
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

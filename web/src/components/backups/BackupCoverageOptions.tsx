// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The "Backup coverage" choice of the create and edit schedule forms: all
 * databases (recommended), specific databases, or only the Temps control
 * plane, plus the control-plane switch for the first two.
 *
 * The form owns the state and validation (`@/lib/backup-coverage`); this
 * renders it. `error` is shown under the database picker, which is the only
 * choice that can back up nothing.
 */

import type { ReactNode } from 'react'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Switch } from '@/components/ui/switch'
import type { BackupCoverageMode } from '@/lib/backup-coverage'

interface CoverageOption {
  value: BackupCoverageMode
  label: string
  description: string
}

const COVERAGE_OPTIONS: ReadonlyArray<CoverageOption> = [
  {
    value: 'all',
    label: 'All databases (recommended)',
    description:
      'Back up every database currently on the host — and any new database you create later, automatically.',
  },
  {
    value: 'specific',
    label: 'Specific databases',
    description:
      'Pick the databases this schedule should back up. New databases are not included unless you attach them.',
  },
  {
    value: 'control_plane',
    label: 'Control plane only',
    description:
      "Only Temps's own database: projects, environment variables, domains and users, what you need to restore this instance. For databases backed up elsewhere, or a more frequent schedule.",
  },
]

function isCoverageMode(value: string): value is BackupCoverageMode {
  return COVERAGE_OPTIONS.some((option) => option.value === value)
}

export interface BackupCoverageOptionsProps {
  /** Prefix for element ids, so two forms never share one (`''`, `'edit-'`). */
  idPrefix: string
  mode: BackupCoverageMode
  onModeChange: (mode: BackupCoverageMode) => void
  includeControlPlane: boolean
  onIncludeControlPlaneChange: (include: boolean) => void
  /** Why the coverage cannot be saved. */
  error?: string
  disabled?: boolean
  /** The database picker, shown for "Specific databases". */
  servicesSelector: ReactNode
}

export function BackupCoverageOptions({
  idPrefix,
  mode,
  onModeChange,
  includeControlPlane,
  onIncludeControlPlaneChange,
  error,
  disabled,
  servicesSelector,
}: BackupCoverageOptionsProps) {
  const servicesId = `${idPrefix}schedule-services`
  const errorId = `${servicesId}-error`
  return (
    <div className="grid gap-2">
      <Label>Backup targets</Label>
      <RadioGroup
        value={mode}
        onValueChange={(value) => {
          if (isCoverageMode(value)) onModeChange(value)
        }}
        className="gap-4"
      >
        {COVERAGE_OPTIONS.map((option) => {
          const id = `${idPrefix}mode-${option.value}`
          return (
            <div
              key={option.value}
              className="flex items-start space-x-3 space-y-0"
            >
              <RadioGroupItem value={option.value} id={id} />
              <div className="grid gap-1 leading-none">
                <Label
                  htmlFor={id}
                  className="text-sm font-medium leading-none"
                >
                  {option.label}
                </Label>
                <p className="text-sm text-muted-foreground">
                  {option.description}
                </p>
              </div>
            </div>
          )
        })}
      </RadioGroup>
      {mode === 'specific' && (
        <div
          id={servicesId}
          role="group"
          aria-label="Databases to back up"
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          tabIndex={-1}
          className="mt-2 rounded-md border p-2 aria-invalid:border-destructive"
        >
          {servicesSelector}
        </div>
      )}
      {mode === 'specific' && error && (
        <p id={errorId} className="text-sm font-medium text-destructive">
          {error}
        </p>
      )}
      {mode !== 'control_plane' && (
        <div className="mt-3 flex items-start justify-between gap-3 rounded-md border p-3">
          <div className="grid gap-1 leading-tight">
            <Label
              htmlFor={`${idPrefix}include-control-plane`}
              className="text-sm font-medium"
            >
              Also back up the Temps control plane
            </Label>
            <p className="text-xs text-muted-foreground">
              Includes Temps&apos;s own database (users, projects, service
              configs, audit logs, error groups). Recommended unless you use
              Temps purely as a backup orchestrator for external databases.
            </p>
          </div>
          <Switch
            id={`${idPrefix}include-control-plane`}
            checked={includeControlPlane}
            onCheckedChange={onIncludeControlPlaneChange}
            disabled={disabled}
          />
        </div>
      )}
    </div>
  )
}

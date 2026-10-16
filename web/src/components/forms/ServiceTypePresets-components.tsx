// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { AlertTriangle } from 'lucide-react'
import { bucketEnvVarName } from '@/lib/s3-buckets'
import { type PresetGroupProps } from './ServiceTypePresets-shared'

export function PresetGroup({
  label,
  description,
  options,
  selected,
  customValue,
  onSelect,
  onCustomChange,
  customPlaceholder,
  pitrWarning,
  pitrManagedImage,
}: PresetGroupProps) {
  const selectedOption = options.find((o) => o.id === selected)
  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <Label>{label}</Label>
        {description && (
          <p className="text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      <div
        className={`grid gap-2 ${
          options.length <= 3
            ? 'grid-cols-1 sm:grid-cols-3'
            : 'grid-cols-2 sm:grid-cols-4'
        }`}
      >
        {options.map((opt) => {
          const isSelected = opt.id === selected
          return (
            <button
              key={opt.id}
              type="button"
              onClick={() => onSelect(opt.id)}
              className={`flex flex-col gap-0.5 rounded-lg border-2 p-3 text-left transition-colors ${
                isSelected
                  ? 'border-primary bg-primary/5'
                  : 'border-border hover:border-muted-foreground/50'
              }`}
            >
              <span className="text-sm font-medium">{opt.title}</span>
              {opt.subtitle && (
                <span className="text-xs text-muted-foreground">
                  {opt.subtitle}
                </span>
              )}
              {opt.hint && (
                <span className="mt-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
                  {opt.hint}
                </span>
              )}
            </button>
          )
        })}
      </div>
      {selectedOption?.custom && (
        <Input
          value={customValue ?? ''}
          onChange={(e) => onCustomChange(e.target.value)}
          placeholder={customPlaceholder}
          autoComplete="off"
        />
      )}
      {pitrWarning && (
        <div className="flex items-start gap-2 rounded-md border border-amber-500/20 bg-amber-500/10 p-3 text-sm text-amber-800 dark:text-amber-200">
          <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5" />
          <div className="space-y-1">
            <p className="font-medium">Point-in-time recovery not available</p>
            <p className="text-xs">
              This image does not include WAL-G. Backups will be basic snapshots
              — you won&apos;t be able to restore to a specific timestamp.
              {pitrManagedImage && (
                <>
                  {' '}
                  For full PITR support, use{' '}
                  <code className="font-mono bg-amber-500/10 px-1 rounded">
                    {pitrManagedImage}
                  </code>
                  .
                </>
              )}
            </p>
          </div>
        </div>
      )}
    </div>
  )
}

interface BucketListFieldProps {
  value: string
  onChange: (value: string) => void
  /** The names `value` parses to, already normalised. */
  buckets: string[]
  error: string | null
}

/**
 * Buckets every environment that links the service receives. Left empty, each
 * environment gets one bucket named after the project and environment.
 */
export function BucketListField({
  value,
  onChange,
  buckets,
  error,
}: BucketListFieldProps) {
  return (
    <div className="space-y-2">
      <div className="space-y-1">
        <Label htmlFor="s3-buckets">Buckets</Label>
        <p className="text-sm text-muted-foreground">
          Comma-separated names, for example{' '}
          <code className="font-mono text-xs">uploads, exports</code>. Each
          linked environment gets its own copy of every bucket and its own
          access key. Leave empty for one bucket per environment.
        </p>
      </div>
      <Input
        id="s3-buckets"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="uploads, exports"
        autoComplete="off"
        aria-invalid={error !== null}
        aria-describedby="s3-buckets-feedback"
      />
      <div id="s3-buckets-feedback">
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : (
          buckets.length > 0 && (
            <ul className="flex flex-wrap gap-1.5">
              {buckets.map((bucket) => (
                <li
                  key={bucket}
                  className="rounded-md bg-muted px-2 py-0.5 font-mono text-xs"
                  title={`Injected into the app as ${bucketEnvVarName(bucket)}`}
                >
                  {bucketEnvVarName(bucket)}
                </li>
              ))}
            </ul>
          )
        )}
      </div>
    </div>
  )
}

import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { AlertTriangle } from 'lucide-react'
import type { ReactNode } from 'react'
import { useState } from 'react'

/**
 * Preset option card. Used by every service preset to pick a common config
 * (typically an image / version).
 */
interface PresetOption {
  id: string
  title: string
  subtitle?: string
  /** Value this option maps to when selected (usually a Docker image ref). */
  value?: string
  /** If true, selecting reveals a free-text input to capture a custom value. */
  custom?: boolean
  /** Optional hint line shown under the subtitle. */
  hint?: string
  /**
   * When true, this option includes WAL-G for point-in-time recovery.
   * When false/undefined, selecting this image means backups will be
   * basic snapshots only (no PITR).
   */
  supportsPitr?: boolean
}

interface PresetGroupProps {
  label: string
  description?: string
  options: PresetOption[]
  selected: string
  customValue?: string
  onSelect: (id: string) => void
  onCustomChange: (value: string) => void
  customPlaceholder?: string
  /**
   * When true, a warning banner is shown explaining that the selected option
   * doesn't support point-in-time recovery (no WAL-G in the image).
   */
  pitrWarning?: boolean
  /** Human name for the managed/recommended image, shown in the warning. */
  pitrManagedImage?: string
}

function PresetGroup({
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
                <span className="mt-0.5 text-[10px] uppercase tracking-wide text-muted-foreground/70">
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
            <p className="font-medium">
              Point-in-time recovery not available
            </p>
            <p className="text-xs">
              This image does not include WAL-G. Backups will be basic
              snapshots — you won't be able to restore to a specific
              timestamp.
              {pitrManagedImage && (
                <>
                  {' '}For full PITR support, use{' '}
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

/** Map every service type to the fields its preset controls. */
export interface PresetState {
  /**
   * Field values the preset produces. `undefined` means the preset owns the
   * field but has no value yet — form should omit it from submission.
   */
  overrides: Record<string, string | undefined>
  /** Field names the preset owns (hidden from the form regardless of value). */
  ownedFields: string[]
  /** React node to render above the JsonSchemaForm. */
  ui: ReactNode
}

/**
 * Renders a per-service-type preset (image pills, persistence toggle, etc.)
 * and returns the field overrides to merge into form submission.
 *
 * Returns null ui + empty overrides for service types that don't have a preset.
 */
export function useServiceTypePreset(
  serviceType: string | null
): PresetState {
  // One hook call per possible preset keeps hook order stable.
  const postgres = usePostgresPreset()
  const mariadb = useMariDbPreset()
  const redis = useRedisPreset()
  const mongodb = useMongodbPreset()
  const s3 = useS3Preset()

  switch (serviceType) {
    case 'postgres':
      return postgres
    case 'mariadb':
      return mariadb
    case 'redis':
      return redis
    case 'mongodb':
      return mongodb
    case 's3':
    case 'rustfs':
    case 'minio':
      return s3
    default:
      return { overrides: {}, ownedFields: [], ui: null }
  }
}

// -----------------------------------------------------------------------------
// MariaDB preset — official MariaDB LTS image + custom.
// -----------------------------------------------------------------------------

const MARIADB_MANAGED_IMAGE = 'mariadb:lts'

const MARIADB_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'MariaDB LTS',
    subtitle: 'Official image',
    value: MARIADB_MANAGED_IMAGE,
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'MariaDB-compatible',
    custom: true,
  },
]

function useMariDbPreset(): PresetState {
  const [selected, setSelected] = useState('managed')
  const [custom, setCustom] = useState('')
  const option = MARIADB_OPTIONS.find((o) => o.id === selected)
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
  }

  return {
    overrides,
    ownedFields: ['docker_image'],
    ui: (
      <PresetGroup
        label="MariaDB version"
        description="Create a shared MariaDB server. Linked projects get separate databases inside it; use the size profile below to tune the container for the host."
        options={MARIADB_OPTIONS}
        selected={selected}
        customValue={custom}
        onSelect={setSelected}
        onCustomChange={setCustom}
        customPlaceholder="e.g. mariadb:11"
      />
    ),
  }
}

// -----------------------------------------------------------------------------
// Postgres preset — only the managed walg image + custom (with PITR warning).
// -----------------------------------------------------------------------------

const POSTGRES_MANAGED_IMAGE = 'gotempsh/postgres-walg:18-bookworm'

const POSTGRES_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'PostgreSQL 18',
    subtitle: 'Managed + WAL-G',
    value: POSTGRES_MANAGED_IMAGE,
    hint: 'PITR ready',
    supportsPitr: true,
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'No PITR',
    custom: true,
  },
]

function usePostgresPreset(): PresetState {
  const [selected, setSelected] = useState('managed')
  const [custom, setCustom] = useState('')
  const option = POSTGRES_OPTIONS.find((o) => o.id === selected)
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
  }

  return {
    overrides,
    ownedFields: ['docker_image'],
    ui: (
      <PresetGroup
        label="PostgreSQL version"
        description="The managed image bundles WAL-G for point-in-time recovery. Custom images only support basic snapshot backups."
        options={POSTGRES_OPTIONS}
        selected={selected}
        customValue={custom}
        onSelect={setSelected}
        onCustomChange={setCustom}
        customPlaceholder="e.g. postgres:17-alpine"
        pitrWarning={!option?.supportsPitr}
        pitrManagedImage={POSTGRES_MANAGED_IMAGE}
      />
    ),
  }
}

// -----------------------------------------------------------------------------
// Redis preset — managed walg image (S3-archived RDB snapshots) + custom.
// PITR is not implemented for Redis; restore is always LATEST.
// -----------------------------------------------------------------------------

const REDIS_MANAGED_IMAGE = 'gotempsh/redis-walg:8-bookworm'

const REDIS_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'Redis 8',
    subtitle: 'Managed + WAL-G',
    value: REDIS_MANAGED_IMAGE,
    hint: 'S3 backups',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'Local snapshots only',
    custom: true,
  },
]

function useRedisPreset(): PresetState {
  const [selected, setSelected] = useState('managed')
  const [custom, setCustom] = useState('')
  const option = REDIS_OPTIONS.find((o) => o.id === selected)
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
  }

  return {
    overrides,
    ownedFields: ['docker_image'],
    ui: (
      <PresetGroup
        label="Redis version"
        description="The managed image bundles WAL-G to push RDB snapshots to S3. Custom images only support local snapshots."
        options={REDIS_OPTIONS}
        selected={selected}
        customValue={custom}
        onSelect={setSelected}
        onCustomChange={setCustom}
        customPlaceholder="e.g. redis:7.2-alpine"
      />
    ),
  }
}

// -----------------------------------------------------------------------------
// MongoDB preset — managed walg image (S3-archived mongodump) + custom.
// PITR is not implemented for MongoDB; restore is always LATEST.
// -----------------------------------------------------------------------------

const MONGO_MANAGED_IMAGE = 'gotempsh/mongodb-walg:8.0'

const MONGO_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'MongoDB 8',
    subtitle: 'Managed + WAL-G',
    value: MONGO_MANAGED_IMAGE,
    hint: 'S3 backups',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'Local dumps only',
    custom: true,
  },
]

function useMongodbPreset(): PresetState {
  const [selected, setSelected] = useState('managed')
  const [custom, setCustom] = useState('')
  const option = MONGO_OPTIONS.find((o) => o.id === selected)
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
  }

  return {
    overrides,
    ownedFields: ['docker_image'],
    ui: (
      <PresetGroup
        label="MongoDB version"
        description="The managed image bundles WAL-G to push mongodump snapshots to S3. Custom images only support local dumps."
        options={MONGO_OPTIONS}
        selected={selected}
        customValue={custom}
        onSelect={setSelected}
        onCustomChange={setCustom}
        customPlaceholder="e.g. mongo:7.0"
      />
    ),
  }
}

// -----------------------------------------------------------------------------
// S3 / RustFS / MinIO preset — engine pills.
// -----------------------------------------------------------------------------

const S3_OPTIONS: PresetOption[] = [
  {
    id: 'rustfs',
    title: 'RustFS',
    subtitle: 'Rust-native',
    value: 'rustfs/rustfs:1.0.0-alpha.98',
    hint: 'Default',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'Bring your own',
    custom: true,
  },
]

function useS3Preset(): PresetState {
  const [selected, setSelected] = useState('rustfs')
  const [custom, setCustom] = useState('')
  const option = S3_OPTIONS.find((o) => o.id === selected)
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
  }

  return {
    overrides,
    ownedFields: ['docker_image'],
    ui: (
      <PresetGroup
        label="Storage engine"
        description="RustFS is the default high-performance Rust-native S3-compatible storage engine."
        options={S3_OPTIONS}
        selected={selected}
        customValue={custom}
        onSelect={setSelected}
        onCustomChange={setCustom}
        customPlaceholder="e.g. rustfs/rustfs:latest"
      />
    ),
  }
}

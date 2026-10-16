// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  DEFAULT_GARAGE_IMAGE,
  DEFAULT_RUSTFS_IMAGE,
} from '@/lib/service-images'
import {
  bucketNamesError,
  bucketsParameter,
  parseBucketInput,
} from '@/lib/s3-buckets'
import type { ReactNode } from 'react'
import { useState } from 'react'
import { BucketListField, PresetGroup } from './ServiceTypePresets-components'

/**
 * Preset option card. Used by every service preset to pick a common config
 * (typically an image / version).
 */
export interface PresetOption {
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

export interface PresetGroupProps {
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
  /**
   * Storage engine the preset selected, for service types that have several.
   * The creation schema depends on it, so the page asks for the matching one.
   */
  backend?: string
  /** A reason the current selection cannot be submitted, shown by the preset. */
  error?: string | null
}

/**
 * Renders a per-service-type preset (image pills, persistence toggle, etc.)
 * and returns the field overrides to merge into form submission.
 *
 * Returns null ui + empty overrides for service types that don't have a preset.
 */
export function useServiceTypePreset(serviceType: string | null): PresetState {
  // One hook call per possible preset keeps hook order stable.
  const postgres = usePostgresPreset()
  const mariadb = useMariDbPreset()
  const redis = useRedisPreset()
  const mongodb = useMongodbPreset()
  const s3 = useS3Preset(serviceType)

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
// MariaDB preset — managed WAL-G image + custom.
// -----------------------------------------------------------------------------

// Pinned to the digest printed by .github/workflows/mariadb-walg-image.yml's
// first run on main (11.4.12-walg-v3.0.8) -- validate_immutable_mariadb_image
// in mariadb.rs requires a real repository@sha256:<digest> reference for any
// non-default MariaDB image, so a mutable tag here is rejected at creation.
export const MARIADB_MANAGED_IMAGE =
  'ghcr.io/gotempsh/mariadb-walg@sha256:fa4c9247f82c47ace7c1aa9b77010870f4905bbae57c4f0aa24ae6ba3b6cdbf3'

export const MARIADB_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'MariaDB 11.4',
    subtitle: 'Managed + WAL-G',
    value: MARIADB_MANAGED_IMAGE,
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'MariaDB-compatible',
    custom: true,
  },
]

export function useMariDbPreset(): PresetState {
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

export const POSTGRES_MANAGED_IMAGE = 'gotempsh/postgres-walg:18-bookworm'

export const POSTGRES_OPTIONS: PresetOption[] = [
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

export function usePostgresPreset(): PresetState {
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
// Redis preset — managed image (RDB snapshots streamed to S3 by its bundled
// WAL-G) + custom (snapshot staged on local disk, then uploaded).
// Postgres-style "WAL-G / PITR" wording is deliberately avoided here: Redis
// has no WAL, and users read "WAL-G" as a PostgreSQL feature.
// PITR is not implemented for Redis; restore is always LATEST.
// -----------------------------------------------------------------------------

export const REDIS_MANAGED_IMAGE = 'gotempsh/redis-walg:8-bookworm'

export const REDIS_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'Redis 8',
    subtitle: 'Managed image',
    value: REDIS_MANAGED_IMAGE,
    hint: 'RDB snapshots streamed to S3',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'Snapshots staged on disk',
    custom: true,
  },
]

export function useRedisPreset(): PresetState {
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
        description="Backups are RDB snapshots uploaded to your S3 backup destination. The managed image streams each snapshot straight to S3; custom images first write a full snapshot copy to local disk, then upload it."
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
// MongoDB preset — managed image (mongodump streamed to S3 by its bundled
// WAL-G) + custom (dump staged on local disk, then uploaded).
// PITR is not implemented for MongoDB; restore is always LATEST.
// -----------------------------------------------------------------------------

export const MONGO_MANAGED_IMAGE = 'gotempsh/mongodb-walg:8.0'

export const MONGO_OPTIONS: PresetOption[] = [
  {
    id: 'managed',
    title: 'MongoDB 8',
    subtitle: 'Managed image',
    value: MONGO_MANAGED_IMAGE,
    hint: 'Dumps streamed to S3',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'Dumps staged on disk',
    custom: true,
  },
]

export function useMongodbPreset(): PresetState {
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
        description="Backups are mongodump archives uploaded to your S3 backup destination. The managed image streams each dump straight to S3; custom images first write the full dump to local disk, then upload it."
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
// S3 / RustFS preset — storage engine cards plus the buckets every linked
// environment receives.
// -----------------------------------------------------------------------------

export const S3_OPTIONS: PresetOption[] = [
  {
    id: 'rustfs',
    title: 'RustFS',
    subtitle: 'Full-featured, web console',
    value: DEFAULT_RUSTFS_IMAGE,
    hint: 'Default',
  },
  {
    id: 'garage',
    title: 'Garage',
    subtitle: 'Lightweight, S3 API only',
    value: DEFAULT_GARAGE_IMAGE,
    hint: 'Small footprint',
  },
  {
    id: 'custom',
    title: 'Custom image',
    subtitle: 'RustFS-compatible',
    custom: true,
  },
]

/** Garage is offered for `s3` services only; other types keep the RustFS cards. */
export function s3OptionsFor(serviceType: string | null): PresetOption[] {
  return serviceType === 's3'
    ? S3_OPTIONS
    : S3_OPTIONS.filter((option) => option.id !== 'garage')
}

export function useS3Preset(serviceType: string | null): PresetState {
  const [selected, setSelected] = useState('rustfs')
  const [custom, setCustom] = useState('')
  const [bucketText, setBucketText] = useState('')
  const options = s3OptionsFor(serviceType)
  const option = options.find((o) => o.id === selected) ?? options[0]
  const resolved = option?.value ?? (option?.custom ? custom.trim() : '')
  const buckets = parseBucketInput(bucketText)
  const error = bucketNamesError(buckets)
  const backend = option?.id === 'garage' ? 'garage' : 'rustfs'
  const overrides: Record<string, string | undefined> = {
    docker_image: resolved || undefined,
    buckets: error ? undefined : bucketsParameter(buckets),
  }
  if (serviceType === 's3') overrides.backend = backend

  return {
    overrides,
    ownedFields: ['docker_image', 'backend', 'buckets'],
    backend: serviceType === 's3' ? backend : undefined,
    error,
    ui: (
      <div className="space-y-6">
        <PresetGroup
          label="Storage engine"
          description="Pick the S3-compatible engine. It cannot be changed later; data can be migrated to a new service."
          options={options}
          selected={option?.id ?? 'rustfs'}
          customValue={custom}
          onSelect={setSelected}
          onCustomChange={setCustom}
          customPlaceholder="e.g. rustfs/rustfs:1.0.0"
        />
        <BucketListField
          value={bucketText}
          onChange={setBucketText}
          buckets={buckets}
          error={error}
        />
      </div>
    ),
  }
}

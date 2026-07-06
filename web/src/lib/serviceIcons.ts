/**
 * Service-type → Lucide icon mapping.
 *
 * Mirrors the visual vocabulary used in `IntegrationBadge` so a Postgres
 * backup, a Postgres env-var integration, and a Postgres service card all
 * show the same icon. Add new service types here when the backend
 * registers them.
 *
 * Unknown types fall through to `resolvePluginIcon` (which understands
 * kebab-case plugin slugs) and finally to the Puzzle icon, so nothing
 * ever crashes on a missing entry.
 */
import type { ServiceTypeRoute } from '@/api/client'
import {
  Boxes,
  Database,
  HardDrive,
  Layers,
  Leaf,
  Server,
  ServerCog,
  type LucideIcon,
} from 'lucide-react'

import { resolvePluginIcon } from './pluginIcons'

/**
 * Backup engine keys come from `BackupEngine::engine()` (Rust); service
 * types come from `external_services.service_type`. The two vocabularies
 * overlap but aren't identical — `postgres_walg`, `postgres_pgdump`, and
 * `postgres_cluster` all map to the Postgres icon. This helper accepts
 * either form.
 */
export function iconForServiceType(serviceType: string | undefined | null): LucideIcon {
  if (!serviceType) return Database
  const normalized = serviceType.toLowerCase()

  // Exact matches first — order matters because `postgres_cluster`
  // would otherwise hit the `startsWith('postgres')` arm twice.
  if (
    normalized === 'control_plane' ||
    normalized === 'control-plane' ||
    normalized === 'controlplane'
  ) {
    return ServerCog
  }

  if (
    normalized.startsWith('postgres') ||
    normalized === 'postgresql' ||
    normalized === 'mysql' ||
    normalized === 'mariadb' ||
    normalized === 'cockroach' ||
    normalized === 'cockroachdb'
  ) {
    return Database
  }

  if (normalized === 'mongodb' || normalized === 'mongo') {
    return Leaf
  }

  if (normalized === 'redis' || normalized === 'keydb' || normalized === 'valkey') {
    return Server
  }

  if (
    normalized === 's3' ||
    normalized === 's3_mirror' ||
    normalized === 'minio' ||
    normalized === 'rustfs' ||
    normalized === 'blob'
  ) {
    return HardDrive
  }

  if (normalized === 'rabbitmq' || normalized === 'nats' || normalized === 'kafka') {
    return Boxes
  }

  if (normalized === 'clickhouse' || normalized === 'elastic' || normalized === 'opensearch') {
    return Layers
  }

  return resolvePluginIcon(normalized)
}

/**
 * Backup engine / service-type string → `ServiceTypeRoute` for the brand
 * SVG logos exposed by `<ServiceLogo>`. Returns `null` when the engine
 * has no brand logo (e.g. `control_plane`, plugin engines) so the caller
 * can fall back to the generic Lucide icon from `iconForServiceType`.
 *
 * Accepts both backup-engine strings (`postgres_walg`, `s3_mirror`) and
 * service-type strings (`postgres`, `s3`) — the same dual vocabulary
 * `iconForServiceType` handles.
 */
export function serviceTypeRouteForEngine(
  serviceType: string | undefined | null,
): ServiceTypeRoute | null {
  if (!serviceType) return null
  const normalized = serviceType.toLowerCase()

  if (normalized.startsWith('postgres') || normalized === 'postgresql') {
    return 'postgres'
  }

  if (normalized === 'mariadb' || normalized === 'mysql') {
    return 'mariadb'
  }

  if (normalized === 'mongodb' || normalized === 'mongo') {
    return 'mongodb'
  }

  if (normalized === 'redis') {
    return 'redis'
  }

  if (normalized === 's3' || normalized === 's3_mirror') {
    return 's3'
  }

  if (normalized === 'minio') return 'minio'
  if (normalized === 'rustfs') return 'rustfs'
  if (normalized === 'blob') return 'blob'
  if (normalized === 'kv') return 'kv'

  return null
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/* eslint-disable react-refresh/only-export-components -- deterministic proposal parsers are shared by persisted and live approval renderers */

import type { ServiceTypeRoute } from '@/api/client'
import { Button } from '@/components/ui/button'
import { ServiceLogo } from '@/components/ui/service-logo'
import { cn } from '@/lib/utils'
import {
  ArrowRight,
  Database,
  KeyRound,
  Link2,
  Server,
  ShieldCheck,
} from 'lucide-react'
import { Link } from 'react-router'

const SERVICE_TYPES: ReadonlySet<string> = new Set([
  'mariadb',
  'mongodb',
  'postgres',
  'redis',
  's3',
  'kv',
  'blob',
  'rustfs',
  'minio',
])

interface ServicePresentation {
  displayName: string
  accentClassName: string
  parameterOrder: readonly string[]
  secretDescription: string
}

const DATABASE_CREDENTIAL_COPY =
  'Database credentials are generated or resolved by Temps and are never exposed to the AI.'
const STORAGE_CREDENTIAL_COPY =
  'Storage credentials are generated or resolved by Temps and are never exposed to the AI.'

const SERVICE_PRESENTATION: Record<ServiceTypeRoute, ServicePresentation> = {
  mariadb: {
    displayName: 'MariaDB',
    accentClassName: 'text-[#003545] dark:text-[#5ec4b6]',
    parameterOrder: [
      'database',
      'username',
      'size_profile',
      'binlog_archive_interval',
      'host',
      'port',
      'docker_image',
    ],
    secretDescription: DATABASE_CREDENTIAL_COPY,
  },
  mongodb: {
    displayName: 'MongoDB',
    accentClassName: 'text-[#47A248] dark:text-[#68c778]',
    parameterOrder: [
      'database',
      'username',
      'replica_set',
      'host',
      'port',
      'docker_image',
    ],
    secretDescription: DATABASE_CREDENTIAL_COPY,
  },
  postgres: {
    displayName: 'PostgreSQL',
    accentClassName: 'text-[#4169E1] dark:text-[#7c9cff]',
    parameterOrder: [
      'database',
      'username',
      'max_connections',
      'ssl_mode',
      'host',
      'port',
      'docker_image',
    ],
    secretDescription: DATABASE_CREDENTIAL_COPY,
  },
  redis: {
    displayName: 'Redis',
    accentClassName: 'text-[#DC382D] dark:text-[#ff6b61]',
    parameterOrder: ['host', 'port', 'docker_image'],
    secretDescription: DATABASE_CREDENTIAL_COPY,
  },
  kv: {
    displayName: 'Key-value store',
    accentClassName: 'text-[#DC382D] dark:text-[#ff6b61]',
    parameterOrder: ['host', 'port', 'docker_image'],
    secretDescription: DATABASE_CREDENTIAL_COPY,
  },
  s3: {
    displayName: 'S3 / RustFS',
    accentClassName: 'text-[#C72E49] dark:text-[#ef6a82]',
    parameterOrder: ['region', 'host', 'port', 'console_port', 'docker_image'],
    secretDescription: STORAGE_CREDENTIAL_COPY,
  },
  blob: {
    displayName: 'Blob storage',
    accentClassName: 'text-[#C72E49] dark:text-[#ef6a82]',
    parameterOrder: ['region', 'host', 'port', 'console_port', 'docker_image'],
    secretDescription: STORAGE_CREDENTIAL_COPY,
  },
  rustfs: {
    displayName: 'RustFS',
    accentClassName: 'text-[#C72E49] dark:text-[#ef6a82]',
    parameterOrder: ['region', 'host', 'port', 'console_port', 'docker_image'],
    secretDescription: STORAGE_CREDENTIAL_COPY,
  },
  minio: {
    displayName: 'MinIO',
    accentClassName: 'text-[#C72E49] dark:text-[#ef6a82]',
    parameterOrder: ['region', 'host', 'port', 'docker_image'],
    secretDescription: STORAGE_CREDENTIAL_COPY,
  },
}

const PARAMETER_LABELS: Record<string, string> = {
  binlog_archive_interval: 'Archive interval',
  console_port: 'Console port',
  docker_image: 'Container image',
  max_connections: 'Maximum connections',
  replica_set: 'Replica set',
  size_profile: 'Size profile',
  ssl_mode: 'SSL mode',
}

const PRIVATE_KEY_PATTERN = /(password|secret|token|credential|private.?key)/i

export interface ServiceProposalField {
  key: string
  label: string
  value: string
}

export interface ServiceProposalViewModel {
  serviceType: ServiceTypeRoute
  serviceName: string
  displayName: string
  accentClassName: string
  version: string | null
  topology: string
  placement: string
  fields: ServiceProposalField[]
  secretsProtected: boolean
  secretDescription: string
}

/** Read the created service id from the trusted action result. A numeric-only
 * id keeps routing independent from model-authored text and prevents arbitrary
 * paths from becoming links. */
export function createdServiceId(resultJson: string | null): number | null {
  if (!resultJson) return null
  try {
    const result: unknown = JSON.parse(resultJson)
    if (!isRecord(result)) return null
    const id = result.id
    return typeof id === 'number' && Number.isSafeInteger(id) && id > 0
      ? id
      : null
  } catch {
    return null
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function isServiceType(value: unknown): value is ServiceTypeRoute {
  return typeof value === 'string' && SERVICE_TYPES.has(value)
}

function labelForKey(key: string): string {
  if (PARAMETER_LABELS[key]) return PARAMETER_LABELS[key]
  return key
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (character) => character.toUpperCase())
}

function displayValue(value: unknown): string | null {
  if (typeof value === 'string') return value || null
  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value)
  }
  return null
}

/**
 * Converts the server-validated `create_service` request into a presentation
 * model. Secret-shaped fields are excluded even though the endpoint already
 * redacts them; the UI keeps its own defensive boundary.
 */
export function serviceProposalViewModel(
  paramsJson: string | null
): ServiceProposalViewModel | null {
  if (!paramsJson) return null

  let params: unknown
  try {
    params = JSON.parse(paramsJson)
  } catch {
    return null
  }

  if (!isRecord(params) || !isServiceType(params.service_type)) return null

  const presentation = SERVICE_PRESENTATION[params.service_type]
  const parameters = isRecord(params.parameters) ? params.parameters : {}
  const fields: ServiceProposalField[] = []
  const seen = new Set<string>()

  for (const key of presentation.parameterOrder) {
    const value = displayValue(parameters[key])
    if (value && !PRIVATE_KEY_PATTERN.test(key)) {
      fields.push({ key, label: labelForKey(key), value })
      seen.add(key)
    }
  }

  for (const [key, rawValue] of Object.entries(parameters)) {
    if (seen.has(key) || PRIVATE_KEY_PATTERN.test(key)) continue
    const value = displayValue(rawValue)
    if (value) fields.push({ key, label: labelForKey(key), value })
  }

  const topology =
    displayValue(params.topology) === 'cluster' ? 'Cluster' : 'Standalone'
  const nodeId = displayValue(params.node_id)

  return {
    serviceType: params.service_type,
    serviceName: displayValue(params.name) ?? 'Untitled service',
    displayName: presentation.displayName,
    accentClassName: presentation.accentClassName,
    version: displayValue(params.version),
    topology,
    placement: nodeId ? `Node ${nodeId}` : 'Automatic placement',
    fields,
    secretsProtected: true,
    secretDescription: presentation.secretDescription,
  }
}

export function GeneratedServiceProposal({
  proposal,
  summary,
  statusLabel,
  statusClassName,
  serviceId,
  projectId,
}: {
  proposal: ServiceProposalViewModel
  summary?: string
  statusLabel: string
  statusClassName?: string
  serviceId?: number | null
  projectId?: number | null
}) {
  return (
    <div className="@container min-w-0">
      <div className="flex min-w-0 items-start gap-3 px-3 py-3 sm:px-4">
        <div
          className={cn(
            'flex size-11 shrink-0 items-center justify-center rounded-lg border bg-background',
            proposal.accentClassName
          )}
        >
          <ServiceLogo service={proposal.serviceType} className="size-6" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            <h3 className="truncate text-base font-semibold sm:text-sm">
              {serviceId
                ? `${proposal.displayName} created`
                : `Create ${proposal.displayName}`}
            </h3>
            {proposal.version && (
              <span className="rounded-full border bg-background px-2 py-0.5 text-xs font-medium text-muted-foreground">
                Version {proposal.version}
              </span>
            )}
          </div>
          <p className="mt-0.5 truncate font-mono text-sm text-foreground sm:text-xs">
            {proposal.serviceName}
          </p>
          {summary && (
            <p className="mt-1 text-sm text-muted-foreground sm:text-xs">
              {summary}
            </p>
          )}
          <div
            className={cn(
              'mt-1.5 flex items-center gap-1.5 text-xs font-medium',
              statusClassName
            )}
          >
            <ShieldCheck className="size-4 shrink-0" />
            {statusLabel}
          </div>
        </div>
      </div>

      <dl className="grid min-w-0 grid-cols-1 border-t bg-background/40 @sm:grid-cols-2">
        <ProposalValue label="Topology" value={proposal.topology} />
        <ProposalValue label="Placement" value={proposal.placement} />
        {projectId != null && (
          <ProposalValue
            label="Target project"
            value={`Project ${projectId}`}
            mono
          />
        )}
        {proposal.fields.map((field) => (
          <ProposalValue
            key={field.key}
            label={field.label}
            value={field.value}
            mono
          />
        ))}
      </dl>

      {proposal.secretsProtected && (
        <div className="flex items-start gap-2 border-t px-3 py-2.5 text-sm text-muted-foreground sm:px-4 sm:text-xs">
          <KeyRound className="mt-0.5 size-4 shrink-0 text-green-600 dark:text-green-400" />
          <span>{proposal.secretDescription}</span>
        </div>
      )}

      {serviceId && (
        <div className="flex items-center border-t px-3 py-2.5 sm:px-4">
          <Button asChild size="sm" variant="outline" className="h-8">
            <Link to={`/storage/${serviceId}`}>
              View service
              <ArrowRight aria-hidden="true" />
            </Link>
          </Button>
        </div>
      )}
    </div>
  )
}

export interface ServiceLinkProposalViewModel {
  serviceId: number
  projectId: number
}

/** Parse the validated identifiers used by `link_service_to_project`. */
export function serviceLinkProposalViewModel(
  params: unknown
): ServiceLinkProposalViewModel | null {
  if (!isRecord(params)) return null
  const serviceId = params.id
  const projectId = params.project_id
  if (
    typeof serviceId !== 'number' ||
    !Number.isSafeInteger(serviceId) ||
    serviceId <= 0 ||
    typeof projectId !== 'number' ||
    !Number.isSafeInteger(projectId) ||
    projectId <= 0
  ) {
    return null
  }
  return { serviceId, projectId }
}

export function GeneratedServiceLinkProposal({
  proposal,
}: {
  proposal: ServiceLinkProposalViewModel
}) {
  return (
    <div className="@container min-w-0">
      <div className="flex min-w-0 items-start gap-3 px-3 py-3 sm:px-4">
        <div className="flex size-11 shrink-0 items-center justify-center rounded-lg border bg-background text-primary">
          <Database className="size-6" aria-hidden="true" />
        </div>
        <div className="min-w-0 flex-1">
          <h3 className="text-base font-semibold sm:text-sm">
            Link existing database
          </h3>
          <p className="mt-1 text-sm text-muted-foreground sm:text-xs">
            Make this database available to the selected project and its
            application workspace.
          </p>
          <div className="mt-1.5 flex items-center gap-1.5 text-xs font-medium text-amber-600 dark:text-amber-400">
            <ShieldCheck className="size-4 shrink-0" />
            Awaiting your approval
          </div>
        </div>
      </div>
      <dl className="grid min-w-0 grid-cols-1 border-t bg-background/40 @sm:grid-cols-2">
        <ProposalValue
          label="Database service"
          value={`Service ${proposal.serviceId}`}
          mono
        />
        <ProposalValue
          label="Target project"
          value={`Project ${proposal.projectId}`}
          mono
        />
      </dl>
      <div className="flex items-start gap-2 border-t px-3 py-2.5 text-sm text-muted-foreground sm:px-4 sm:text-xs">
        <Link2 className="mt-0.5 size-4 shrink-0 text-green-600 dark:text-green-400" />
        <span>
          Temps will refresh workspace networking and project context after the
          link is created.
        </span>
      </div>
    </div>
  )
}

function ProposalValue({
  label,
  value,
  mono = false,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className="min-w-0 border-b px-3 py-2.5 last:border-b-0 @sm:[&:nth-last-child(-n+2)]:border-b-0 @sm:[&:nth-child(odd)]:border-r sm:px-4">
      <dt className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        {label === 'Placement' && <Server className="size-3.5 shrink-0" />}
        {label}
      </dt>
      <dd
        className={cn(
          'mt-1 min-w-0 truncate text-sm font-medium sm:text-xs',
          mono && 'font-mono'
        )}
        title={value}
      >
        {value}
      </dd>
    </div>
  )
}

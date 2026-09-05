// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Boxes,
  CheckCircle2,
  Circle,
  KeyRound,
  ListChecks,
  Table2,
} from 'lucide-react'
import type { ThreadArtifactResponse } from '@/api/client'
import { GeneratedDeploymentCard } from '@/components/ai/GeneratedDeploymentCard'
import { GeneratedProjectCollection } from '@/components/ai/GeneratedProjectCollection'

function stringValue(value: unknown): string {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean')
    return String(value)
  return ''
}

function rows(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value)
    ? value.filter(
        (item): item is Record<string, unknown> =>
          typeof item === 'object' && item !== null && !Array.isArray(item)
      )
    : []
}

function objectPayload(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {}
}

function positiveInteger(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0
    ? value
    : null
}

function ProjectCollectionArtifact({
  artifact,
  payload,
}: {
  artifact: ThreadArtifactResponse
  payload: Record<string, unknown>
}) {
  const items = rows(payload.items ?? payload.rows)
  const projects = items.flatMap((item) => {
    const id = positiveInteger(item.id ?? item.project_id)
    const name = stringValue(item.name)
    const slug = stringValue(item.slug)
    return name
      ? [
          {
            id,
            name,
            slug,
            repoName: stringValue(item.repo_name) || undefined,
            repoOwner: stringValue(item.repo_owner) || undefined,
            preset: stringValue(item.preset) || undefined,
          },
        ]
      : []
  })

  return (
    <GeneratedProjectCollection
      title={artifact.title ?? 'Projects'}
      presentation={{
        items: projects,
        total: projects.length,
        page: 1,
        perPage: projects.length,
      }}
    />
  )
}

export function ArtifactRenderer({
  artifact,
}: {
  artifact: ThreadArtifactResponse
}) {
  const payload = objectPayload(artifact.payload)
  const resourceType = stringValue(
    payload.resource_type ?? payload.resourceType ?? payload.type
  ).toLowerCase()
  if (
    artifact.kind === 'collection' &&
    (resourceType === 'project' || resourceType === 'projects')
  ) {
    return <ProjectCollectionArtifact artifact={artifact} payload={payload} />
  }
  if (
    (artifact.kind === 'resource' || artifact.kind === 'operation') &&
    resourceType === 'deployment'
  ) {
    const reference = objectPayload(payload.reference)
    const attributes = objectPayload(payload.attributes)
    return (
      <section className="overflow-hidden rounded-lg border border-blue-500/30 bg-blue-500/5">
        <GeneratedDeploymentCard
          paramsJson={JSON.stringify({ ...attributes, ...reference })}
          resultJson={JSON.stringify({ ...attributes, ...reference })}
          actionStatus="executed"
          createdAt={artifact.created_at}
          summary={stringValue(payload.summary) || undefined}
          statusLabel="Queued"
          statusClassName="text-blue-600 dark:text-blue-400"
        />
      </section>
    )
  }
  const payloadRows = rows(
    payload.nodes ??
      payload.steps ??
      payload.requirements ??
      payload.rows ??
      payload.fields
  )
  const Icon =
    artifact.kind === 'topology'
      ? Boxes
      : artifact.kind === 'execution_plan'
        ? ListChecks
        : artifact.kind === 'credential_request'
          ? KeyRound
          : artifact.kind === 'table'
            ? Table2
            : CheckCircle2

  return (
    <section className="rounded-lg border border-border bg-background p-4">
      <div className="flex items-center gap-2">
        <Icon className="size-4 stroke-success" />
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">
            {artifact.title ?? artifact.kind.replace(/_/g, ' ')}
          </p>
          <p className="font-mono text-[10px] tracking-wide text-muted-foreground">
            Generated view · schema v{artifact.schema_version}
          </p>
        </div>
      </div>
      {payloadRows.length > 0 ? (
        <div className="mt-3 space-y-2">
          {payloadRows.map((row, index) => {
            const label =
              stringValue(row.name) ||
              stringValue(row.label) ||
              stringValue(row.title) ||
              stringValue(row.capability) ||
              `Item ${index + 1}`
            const detail =
              stringValue(row.description) ||
              stringValue(row.status) ||
              stringValue(row.project) ||
              stringValue(row.type)
            return (
              <div
                key={`${label}-${index}`}
                className="flex items-start gap-2 rounded-md border border-border bg-muted/50 px-3 py-2"
              >
                <Circle className="mt-1 size-2.5 shrink-0 stroke-success" />
                <div className="min-w-0">
                  <p className="truncate text-xs text-foreground">{label}</p>
                  {detail && (
                    <p className="mt-0.5 text-[11px] text-muted-foreground">
                      {detail}
                    </p>
                  )}
                </div>
              </div>
            )
          })}
        </div>
      ) : (
        <p className="mt-3 text-xs leading-5 text-muted-foreground">
          {stringValue(payload.summary) || 'This artifact has no list items.'}
        </p>
      )}
    </section>
  )
}

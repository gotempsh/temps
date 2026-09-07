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
import { useState } from 'react'
import { Link } from 'react-router'
import type { ThreadArtifactResponse } from '@/api/client'
import { GeneratedDeploymentCard } from '@/components/ai/GeneratedDeploymentCard'
import { GeneratedProjectCollection } from '@/components/ai/GeneratedProjectCollection'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { cn } from '@/lib/utils'

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

function statusClassName(status: unknown): string {
  const value = stringValue(status).toLowerCase()
  if (
    [
      'success',
      'succeeded',
      'complete',
      'completed',
      'ready',
      'healthy',
    ].includes(value)
  ) {
    return 'text-emerald-600 dark:text-emerald-400'
  }
  if (['warning', 'pending', 'queued', 'running', 'starting'].includes(value)) {
    return 'text-amber-600 dark:text-amber-400'
  }
  if (['error', 'failed', 'failure', 'down', 'unhealthy'].includes(value)) {
    return 'text-red-600 dark:text-red-400'
  }
  return 'text-muted-foreground'
}

function CredentialRequestArtifact({
  artifact,
  payload,
}: {
  artifact: ThreadArtifactResponse
  payload: Record<string, unknown>
}) {
  const requirements = rows(payload.requirements ?? payload.rows)
  const projectId = positiveInteger(payload.project_id)
  const destination = projectId
    ? `/projects/${projectId}/settings/environment-variables`
    : '/projects'

  return (
    <section className="rounded-lg border border-border bg-background p-4">
      <div className="flex items-center gap-2">
        <KeyRound className="size-4 text-amber-600 dark:text-amber-400" />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">
            {artifact.title ?? 'Credentials required'}
          </p>
          <p className="text-xs text-muted-foreground">
            Add values through Temps secret storage. Values are never submitted
            through chat.
          </p>
        </div>
      </div>
      {requirements.length > 0 && (
        <ul className="mt-3 space-y-1 text-xs text-muted-foreground">
          {requirements.map((requirement, index) => (
            <li key={`${stringValue(requirement.name)}-${index}`}>
              {stringValue(requirement.name) ||
                stringValue(requirement.key) ||
                stringValue(requirement.capability) ||
                `Credential ${index + 1}`}
            </li>
          ))}
        </ul>
      )}
      <Button asChild size="sm" className="mt-3">
        <Link to={destination}>Open secret settings</Link>
      </Button>
    </section>
  )
}

function FormArtifact({
  artifact,
  payload,
}: {
  artifact: ThreadArtifactResponse
  payload: Record<string, unknown>
}) {
  const fields = rows(payload.fields)
  const [values, setValues] = useState<Record<string, string>>({})
  const [submitted, setSubmitted] = useState(false)

  return (
    <section className="rounded-lg border border-border bg-background p-4">
      <p className="text-sm font-medium">
        {artifact.title ?? 'Provide details'}
      </p>
      <form
        className="mt-3 space-y-3"
        onSubmit={(event) => {
          event.preventDefault()
          setSubmitted(true)
        }}
      >
        {fields.map((field, index) => {
          const name = stringValue(field.name) || `field_${index + 1}`
          const label = stringValue(field.label) || name
          return (
            <div key={name} className="space-y-1.5">
              <Label htmlFor={`${artifact.public_id}-${name}`}>{label}</Label>
              <Input
                id={`${artifact.public_id}-${name}`}
                name={name}
                required={field.required === true}
                value={values[name] ?? ''}
                onChange={(event) => {
                  setSubmitted(false)
                  setValues((current) => ({
                    ...current,
                    [name]: event.target.value,
                  }))
                }}
              />
            </div>
          )
        })}
        <Button type="submit" size="sm" disabled={fields.length === 0}>
          Submit
        </Button>
        {submitted && (
          <p
            role="status"
            className="text-xs text-emerald-600 dark:text-emerald-400"
          >
            Form response captured for this view.
          </p>
        )}
      </form>
    </section>
  )
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
  if (artifact.kind === 'credential_request') {
    return <CredentialRequestArtifact artifact={artifact} payload={payload} />
  }
  if (artifact.kind === 'form') {
    return <FormArtifact artifact={artifact} payload={payload} />
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
        <Icon className={cn('size-4', statusClassName(artifact.status))} />
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
                <Circle
                  className={cn(
                    'mt-1 size-2.5 shrink-0',
                    statusClassName(row.status)
                  )}
                />
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

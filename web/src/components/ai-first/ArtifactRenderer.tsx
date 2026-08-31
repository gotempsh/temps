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

export function ArtifactRenderer({
  artifact,
}: {
  artifact: ThreadArtifactResponse
}) {
  const payload = objectPayload(artifact.payload)
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

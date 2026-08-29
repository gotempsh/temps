// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getServicePreviewEnvironmentVariableNamesOptions,
  listManagedEnvironmentVariablesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ManagedEnvironmentVariable,
  ManagedEnvironmentVariableSource,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { cn } from '@/lib/utils'
import {
  databaseProvidedEnvironmentVariable,
  findProvidedEnvironmentVariableCollision,
  groupManagedEnvironmentVariables,
  normalizeCreationPreset,
  type ProvidedEnvironmentVariableCollision,
} from '@/lib/provided-environment-variables'
import { useQueries, useQuery } from '@tanstack/react-query'
import {
  Braces,
  ChevronDown,
  Database,
  Loader2,
  LockKeyhole,
  ServerCog,
  TriangleAlert,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

export interface ProvidedEnvironmentVariableDatabase {
  id: number
  name: string
  serviceType: string
}

interface ProvidedEnvironmentVariablesProps {
  preset: string
  databases: ProvidedEnvironmentVariableDatabase[]
  onVariablesChange?: (
    variables: ProvidedEnvironmentVariableCollision[]
  ) => void
}

const SOURCE_DETAILS: Record<
  ManagedEnvironmentVariableSource,
  { label: string; description: string }
> = {
  error_tracking: {
    label: 'Error tracking',
    description: 'Sentry-compatible project and release metadata',
  },
  open_telemetry: {
    label: 'OpenTelemetry',
    description: 'Trace export and service metadata',
  },
  temps: {
    label: 'Temps platform',
    description: 'Deployment APIs and scheduled request authentication',
  },
}

const EMPTY_MANAGED_ENVIRONMENT_VARIABLES: ManagedEnvironmentVariable[] = []

export function ProvidedEnvironmentVariables({
  preset,
  databases,
  onVariablesChange,
}: ProvidedEnvironmentVariablesProps) {
  const [isOpen, setIsOpen] = useState(false)
  const lastReportedVariablesRef = useRef('')
  const normalizedPreset = normalizeCreationPreset(preset)
  const platformQuery = useQuery({
    ...listManagedEnvironmentVariablesOptions({
      query: { preset: normalizedPreset },
    }),
    staleTime: 5 * 60 * 1000,
  })
  const databaseQueries = useQueries({
    queries: databases.map((database) => ({
      ...getServicePreviewEnvironmentVariableNamesOptions({
        path: { id: database.id },
      }),
      staleTime: 5 * 60 * 1000,
    })),
  })

  const platformVariables =
    platformQuery.data ?? EMPTY_MANAGED_ENVIRONMENT_VARIABLES
  const platformGroups = groupManagedEnvironmentVariables(platformVariables)
  const databaseVariableCount = databaseQueries.reduce(
    (count, query) => count + (query.data?.length ?? 0),
    0
  )
  const totalVariableCount = platformVariables.length + databaseVariableCount
  const isLoading =
    platformQuery.isLoading || databaseQueries.some((query) => query.isLoading)
  const collisionVariables = useMemo<ProvidedEnvironmentVariableCollision[]>(
    () => [
      ...platformVariables.map((variable) => ({
        name: variable.name,
        provider: 'Temps',
        isUserOverridable: variable.is_user_overridable,
      })),
      ...databases.flatMap((database, index) =>
        (databaseQueries[index]?.data ?? []).map((name) =>
          databaseProvidedEnvironmentVariable(name, database.name)
        )
      ),
    ],
    [platformVariables, databases, databaseQueries]
  )
  const collisionVariablesSignature = JSON.stringify(collisionVariables)

  useEffect(() => {
    if (
      !onVariablesChange ||
      collisionVariablesSignature === lastReportedVariablesRef.current
    ) {
      return
    }
    lastReportedVariablesRef.current = collisionVariablesSignature
    onVariablesChange(collisionVariables)
  }, [collisionVariables, collisionVariablesSignature, onVariablesChange])

  return (
    <Collapsible open={isOpen} onOpenChange={setIsOpen}>
      <div className="overflow-hidden rounded-lg border border-primary/25 bg-primary/[0.035]">
        <CollapsibleTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            className="h-auto w-full justify-start rounded-none px-4 py-3 text-left hover:bg-primary/[0.06]"
            aria-label={`${isOpen ? 'Hide' : 'Show'} environment variables provided by Temps`}
          >
            <div className="flex w-full items-center gap-3">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-primary/25 bg-background">
                <Braces className="h-4 w-4 text-primary" />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">Provided by Temps</span>
                  {isLoading ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
                  ) : totalVariableCount > 0 ? (
                    <Badge
                      variant="secondary"
                      className="font-mono text-[10px]"
                    >
                      {totalVariableCount} variable
                      {totalVariableCount === 1 ? '' : 's'}
                    </Badge>
                  ) : null}
                </div>
                <p className="mt-0.5 text-xs font-normal text-muted-foreground">
                  Platform and selected database variables are injected
                  automatically. You only need to add app-owned configuration.
                </p>
              </div>
              <ChevronDown
                className={cn(
                  'h-4 w-4 shrink-0 text-muted-foreground transition-transform',
                  isOpen && 'rotate-180'
                )}
              />
            </div>
          </Button>
        </CollapsibleTrigger>

        <CollapsibleContent>
          <div className="border-t border-primary/15 bg-background/65 px-4 py-4">
            {platformQuery.isError ? (
              <LoadError message="Platform variable catalog could not be loaded." />
            ) : platformQuery.isLoading ? (
              <LoadingRow label="Loading platform variables" />
            ) : (
              <div className="space-y-5">
                {platformGroups.map(({ source, variables }) => {
                  const details = SOURCE_DETAILS[source]
                  return (
                    <VariableGroup
                      key={source}
                      icon={<ServerCog className="h-3.5 w-3.5" />}
                      title={details.label}
                      description={details.description}
                      variables={variables}
                    />
                  )
                })}
              </div>
            )}

            <div className="mt-5 border-t pt-5">
              <div className="mb-3 flex items-start gap-2">
                <Database className="mt-0.5 h-3.5 w-3.5 text-muted-foreground" />
                <div>
                  <p className="text-xs font-medium">Selected databases</p>
                  <p className="text-[11px] text-muted-foreground">
                    Connection variable names update with your database
                    selection.
                  </p>
                </div>
              </div>

              {databases.length === 0 ? (
                <p className="rounded-md border border-dashed px-3 py-2.5 text-xs text-muted-foreground">
                  Select a database to include its connection variables here.
                </p>
              ) : (
                <div className="space-y-3">
                  {databases.map((database, index) => {
                    const query = databaseQueries[index]
                    return (
                      <div
                        key={database.id}
                        className="rounded-md border bg-muted/20 px-3 py-3"
                      >
                        <div className="mb-2 flex flex-wrap items-center gap-2">
                          <span className="text-xs font-medium">
                            {database.name}
                          </span>
                          <Badge
                            variant="outline"
                            className="font-mono text-[10px] font-normal"
                          >
                            {database.serviceType}
                          </Badge>
                        </div>
                        {query?.isError ? (
                          <LoadError message="Variable names could not be loaded." />
                        ) : query?.isLoading ? (
                          <LoadingRow label="Loading database variables" />
                        ) : (
                          <div className="flex flex-wrap gap-1.5">
                            {query?.data?.map((name) => (
                              <code
                                key={name}
                                className="rounded border bg-background px-1.5 py-1 text-[11px]"
                              >
                                {name}
                              </code>
                            ))}
                          </div>
                        )}
                      </div>
                    )
                  })}
                </div>
              )}
            </div>
          </div>
        </CollapsibleContent>
      </div>
    </Collapsible>
  )
}

function VariableGroup({
  icon,
  title,
  description,
  variables,
}: {
  icon: React.ReactNode
  title: string
  description: string
  variables: ManagedEnvironmentVariable[]
}) {
  return (
    <section>
      <div className="mb-2 flex items-start gap-2">
        <span className="mt-0.5 text-muted-foreground">{icon}</span>
        <div>
          <p className="text-xs font-medium">{title}</p>
          <p className="text-[11px] text-muted-foreground">{description}</p>
        </div>
      </div>
      <div className="divide-y rounded-md border bg-muted/15">
        {variables.map((variable) => (
          <div
            key={variable.name}
            className="grid gap-1 px-3 py-2.5 sm:grid-cols-[minmax(14rem,auto)_1fr] sm:items-center sm:gap-4"
          >
            <div className="flex min-w-0 items-center gap-1.5">
              <code className="truncate text-[11px] font-medium">
                {variable.name}
              </code>
              {variable.is_secret && (
                <LockKeyhole
                  className="h-3 w-3 shrink-0 text-muted-foreground"
                  aria-label="Secret value"
                />
              )}
            </div>
            <p className="text-[11px] text-muted-foreground sm:text-right">
              {variable.description}
            </p>
          </div>
        ))}
      </div>
    </section>
  )
}

export function ProvidedEnvironmentVariableWarning({
  variableName,
  providedVariables,
}: {
  variableName: string
  providedVariables: ProvidedEnvironmentVariableCollision[]
}) {
  const normalizedName = variableName.trim()
  const collision = findProvidedEnvironmentVariableCollision(
    normalizedName,
    providedVariables
  )

  if (!collision) return null

  return (
    <div
      role="status"
      aria-live="polite"
      className="flex items-start gap-1.5 px-0.5 pt-0.5 text-[11px] leading-4 text-amber-700 dark:text-amber-300"
    >
      <TriangleAlert className="mt-0.5 h-3 w-3 shrink-0 opacity-80" />
      <p>
        <code className="font-mono font-medium">{normalizedName}</code> is
        provided by {collision.provider}.{' '}
        {collision.isUserOverridable
          ? 'Your value takes precedence.'
          : 'Temps will override this value; remove it or use another key.'}
      </p>
    </div>
  )
}

function LoadingRow({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-2 text-xs text-muted-foreground">
      <Loader2 className="h-3.5 w-3.5 animate-spin" />
      {label}
    </div>
  )
}

function LoadError({ message }: { message: string }) {
  return (
    <div className="flex items-center gap-2 text-xs text-destructive">
      <TriangleAlert className="h-3.5 w-3.5" />
      {message}
    </div>
  )
}

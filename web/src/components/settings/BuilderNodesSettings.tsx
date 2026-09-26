// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { BuildNodePolicyResponse, NodeInfoResponse } from '@/api/client'
import { setGlobalBuildNodes, setProjectBuildNodes } from '@/api/client/sdk.gen'
import {
  adminListNodesOptions,
  getApiKeyPermissionsOptions,
  getGlobalBuildNodesOptions,
  getProjectBuildNodesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useAuth } from '@/contexts/AuthContext'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { zodResolver } from '@hookform/resolvers/zod'
import { useEffect, useState } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { ArrowDown, ArrowUp, Server, X } from 'lucide-react'
import {
  Button,
  Callout,
  Field,
  PageHeader,
  PageState,
  Picker,
  Settings,
  SettingsGroup,
  Status,
} from '@temps-sdk/ds'
import {
  Input,
  Label,
  RadioGroup,
  RadioGroupItem,
  Skeleton,
} from '@temps-sdk/ui'
import {
  buildNodeDefaults,
  buildNodeError,
  buildNodeFormSchema,
  buildNodeRequest,
  builderName,
  moveBuilder,
  type BuildNodeFormValues,
} from './build-node-form'

/** Shared global/project surface. The server remains authoritative for authorization. */
export function BuilderNodesSettings({ projectId }: { projectId?: number }) {
  const { user } = useAuth()
  const permissions = useQuery(getApiKeyPermissionsOptions())
  const role = permissions.data?.roles.find((role) => role.name === user?.role)
  const permission =
    projectId === undefined ? 'settings:write' : 'projects:write'
  const canEdit = role?.permissions.includes(permission) ?? false
  const canList = role?.permissions.includes('settings:read') ?? false
  const options =
    projectId === undefined
      ? getGlobalBuildNodesOptions()
      : getProjectBuildNodesOptions({ path: { project_id: projectId } })
  const globalPolicy = useQuery({
    ...getGlobalBuildNodesOptions(),
    enabled: projectId === undefined,
    retry: false,
    refetchOnWindowFocus: true,
  })
  const projectPolicy = useQuery({
    ...getProjectBuildNodesOptions({ path: { project_id: projectId ?? 0 } }),
    enabled: projectId !== undefined,
    retry: false,
    refetchOnWindowFocus: true,
  })
  const policy = projectId === undefined ? globalPolicy : projectPolicy
  const roster = useQuery({
    ...adminListNodesOptions(),
    enabled: canList,
    retry: false,
    refetchInterval: 30_000,
  })
  const queryClient = useQueryClient()
  const save = useMutation({
    mutationFn: async (values: BuildNodeFormValues) => {
      const body = buildNodeRequest(values)
      const response =
        projectId === undefined
          ? await setGlobalBuildNodes({ body, throwOnError: true })
          : await setProjectBuildNodes({
              path: { project_id: projectId },
              body,
              throwOnError: true,
            })
      return response.data
    },
    onSuccess: (data) => {
      queryClient.setQueryData(options.queryKey, data)
      // A global change affects every cached inherited project, not just this page.
      void queryClient.invalidateQueries({
        queryKey: [{ _id: 'getProjectBuildNodes' }],
      })
      void queryClient.invalidateQueries({
        queryKey: getGlobalBuildNodesOptions().queryKey,
      })
    },
  })

  if (!policy.data)
    return (
      <div className="w-full min-w-0 space-y-6">
        <PageHeader
          title={projectId === undefined ? 'Builder nodes' : 'Pipelines'}
          headingLevel={projectId === undefined ? 'h1' : 'h2'}
        />
        {policy.isError ? (
          <PageState
            variant="failed"
            size="compact"
            icon={Server}
            title="Could not load builder settings"
            description={buildNodeError(policy.error)}
            action={
              <Button type="button" onClick={() => void policy.refetch()}>
                Retry builder settings
              </Button>
            }
          />
        ) : (
          <div aria-label="Loading builder settings" className="space-y-4">
            <Skeleton className="h-24 w-full" />
            <Skeleton className="h-48 w-full" />
          </div>
        )}
      </div>
    )

  return (
    <BuilderNodesForm
      key={projectId ?? 'global'}
      policy={policy.data}
      projectId={projectId}
      nodes={roster.data?.nodes ?? []}
      rosterKnown={roster.isSuccess}
      rosterLoading={canList && roster.isPending}
      rosterError={roster.isError}
      canList={canList}
      canEdit={canEdit}
      permissionsPending={permissions.isPending}
      permissionsError={permissions.isError}
      retryPermissions={() => void permissions.refetch()}
      retryRoster={() => void roster.refetch()}
      policyError={policy.isError}
      retryPolicy={() => void policy.refetch()}
      saving={save.isPending}
      saveError={save.error}
      onSave={save.mutateAsync}
    />
  )
}

interface BuilderNodesFormProps {
  policy: BuildNodePolicyResponse
  projectId?: number
  nodes: NodeInfoResponse[]
  rosterKnown: boolean
  rosterLoading: boolean
  rosterError: boolean
  canList: boolean
  canEdit: boolean
  permissionsPending: boolean
  permissionsError: boolean
  retryPermissions: () => void
  retryRoster: () => void
  policyError: boolean
  retryPolicy: () => void
  saving: boolean
  saveError: unknown
  onSave: (values: BuildNodeFormValues) => Promise<BuildNodePolicyResponse>
}

export function BuilderNodesForm(props: BuilderNodesFormProps) {
  const { policy, projectId, nodes, canEdit, saving } = props
  const form = useForm<BuildNodeFormValues>({
    resolver: zodResolver(buildNodeFormSchema),
    defaultValues: buildNodeDefaults(policy),
    mode: 'onChange',
  })
  const mode = useWatch({ control: form.control, name: 'mode' })
  const ids = useWatch({ control: form.control, name: 'ids' })
  const [saved, setSaved] = useState(false)
  const [manualId, setManualId] = useState('')
  const [manualError, setManualError] = useState('')
  const { isDirty, errors } = form.formState
  const { reset } = form
  const workers = nodes.filter((node) => node.id > 0 && node.role === 'worker')

  useEffect(() => {
    // Polls/refetches must never overwrite an in-progress edit.
    if (!isDirty) reset(buildNodeDefaults(policy))
  }, [policy, reset, isDirty])

  const updateIds = (next: number[]) => {
    if (!canEdit || saving) return
    setSaved(false)
    form.setValue('ids', next, { shouldDirty: true, shouldValidate: true })
  }
  const add = (id: number) => {
    if (ids.includes(id) || ids.length >= 100) return
    updateIds([...ids, id])
  }
  const submit = form.handleSubmit(async (values) => {
    if (!canEdit || saving || props.policyError) return
    setSaved(false)
    try {
      const result = await props.onSave(values)
      form.reset(buildNodeDefaults(result))
      setSaved(true)
    } catch {
      /* Mutation state keeps the error visible and draft intact. */
    }
  })
  const missing =
    props.rosterKnown &&
    ids.some((id) => !workers.some((node) => node.id === id))
  const effective = policy.effective_node_ids
  const automatic =
    'Automatic: builds run on the control plane when local builds are enabled; otherwise Temps selects a worker.'

  return (
    <Settings
      embedded
      headingLevel={projectId === undefined ? 'h1' : 'h2'}
      title={projectId === undefined ? 'Builder nodes' : 'Pipelines'}
      description={
        projectId === undefined
          ? 'Default workers for source-image builds across your projects.'
          : 'Choose where this project’s source-image builds run.'
      }
      saving={saving}
      dirty={canEdit && isDirty && !props.policyError}
      errors={{ 'Builder nodes': errors.ids?.message }}
      onSubmit={submit}
      onCancel={() => {
        if (!saving) {
          form.reset(buildNodeDefaults(policy))
          setSaved(false)
        }
      }}
    >
      <div className="max-w-5xl space-y-6">
        {props.policyError && (
          <Callout tone="error" title="Builder settings could not refresh">
            Showing the last loaded policy. Saving is paused until it refreshes.{' '}
            <Button type="button" variant="link" onClick={props.retryPolicy}>
              Retry builder settings
            </Button>
          </Callout>
        )}
        {props.permissionsError ? (
          <Callout tone="error" title="Could not check permissions">
            <Button
              type="button"
              variant="link"
              onClick={props.retryPermissions}
            >
              Retry permissions
            </Button>
          </Callout>
        ) : (
          !canEdit && (
            <Callout>
              {props.permissionsPending
                ? 'Checking your permissions…'
                : `Read-only. Updating this selection requires ${projectId === undefined ? 'settings:write' : 'projects:write'} permission.`}
            </Callout>
          )
        )}
        <SettingsGroup
          title="Builder nodes"
          headingLevel={projectId === undefined ? 'h2' : 'h3'}
          description={
            projectId === undefined
              ? 'Default for projects without an override.'
              : 'Use the global default or choose a dedicated build pool.'
          }
        >
          <div className="space-y-1">
            <p className="text-xs font-medium text-muted-foreground">
              Current policy
            </p>
            <p className="text-sm" data-testid="builder-effective-policy">
              {effective?.length
                ? `${policy.source === 'project' ? 'Project override' : 'Global default'}: ${effective.map((id) => builderName(id, nodes)).join(' → ')}`
                : automatic}
            </p>
          </div>
          <div className="flex flex-wrap gap-4 text-sm">
            {projectId !== undefined && (
              <Link
                to="/settings/build-nodes"
                className="underline underline-offset-4"
              >
                Global builder settings
              </Link>
            )}
            <Link to="/settings/nodes" className="underline underline-offset-4">
              Manage worker nodes
            </Link>
          </div>
          <fieldset disabled={!canEdit || saving} className="min-w-0 space-y-5">
            <Controller
              control={form.control}
              name="mode"
              render={({ field }) => (
                <Field label="Selection mode">
                  {(fieldProps) => (
                    <RadioGroup
                      {...fieldProps}
                      name={field.name}
                      value={field.value}
                      onValueChange={(value) => {
                        field.onChange(value)
                        setSaved(false)
                      }}
                      className="grid gap-3 xl:grid-cols-2"
                    >
                      <Label
                        htmlFor={`${fieldProps.id}-default`}
                        className="flex items-center gap-3 rounded-md border p-4 has-[[data-state=checked]]:border-foreground"
                      >
                        <RadioGroupItem
                          id={`${fieldProps.id}-default`}
                          value="default"
                        />
                        {projectId === undefined
                          ? 'Automatic selection'
                          : 'Inherit global default'}
                      </Label>
                      <Label
                        htmlFor={`${fieldProps.id}-custom`}
                        className="flex items-center gap-3 rounded-md border p-4 has-[[data-state=checked]]:border-foreground"
                      >
                        <RadioGroupItem
                          id={`${fieldProps.id}-custom`}
                          value="custom"
                        />
                        Select worker nodes
                      </Label>
                    </RadioGroup>
                  )}
                </Field>
              )}
            />
            {mode === 'custom' && (
              <div className="space-y-4">
                <p className="text-sm text-muted-foreground">
                  Select one worker to pin builds, or add up to 100 in priority
                  order. Temps uses the first active, architecture-compatible
                  worker. It never falls back outside this pool, even if the
                  control plane can build.
                </p>
                {missing && (
                  <Callout tone="warning">
                    A selected worker is missing or no longer has the worker
                    role. Remove it before saving a custom pool.
                  </Callout>
                )}
                <ol
                  aria-label="Builder priority"
                  className="list-decimal space-y-2 pl-6"
                >
                  {ids.map((id, index) => (
                    <li key={id} className="text-sm tabular-nums">
                      <div className="flex min-w-0 flex-wrap items-center justify-between gap-2 rounded-md border p-3">
                        <div className="min-w-40 flex-1 space-y-1">
                          <p className="break-words font-medium [overflow-wrap:anywhere]">
                            {builderName(id, nodes)}
                          </p>
                          <BuilderMetadata
                            id={id}
                            nodes={nodes}
                            rosterKnown={props.rosterKnown}
                          />
                        </div>
                        <div className="flex gap-1">
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon"
                            aria-label={`Move ${builderName(id, nodes)} up`}
                            disabled={index === 0}
                            onClick={() =>
                              updateIds(moveBuilder(ids, index, -1))
                            }
                          >
                            <ArrowUp className="size-4" aria-hidden />
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon"
                            aria-label={`Move ${builderName(id, nodes)} down`}
                            disabled={index === ids.length - 1}
                            onClick={() =>
                              updateIds(moveBuilder(ids, index, 1))
                            }
                          >
                            <ArrowDown className="size-4" aria-hidden />
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon"
                            aria-label={`Remove ${builderName(id, nodes)}`}
                            onClick={() =>
                              updateIds(ids.filter((value) => value !== id))
                            }
                          >
                            <X className="size-4" aria-hidden />
                          </Button>
                        </div>
                      </div>
                    </li>
                  ))}
                </ol>
                <p className="text-sm text-muted-foreground">
                  {ids.length} / 100 workers selected
                </p>
                {props.rosterLoading ? (
                  <Skeleton
                    aria-label="Loading workers"
                    className="h-36 w-full"
                  />
                ) : props.rosterKnown && workers.length === 0 ? (
                  <PageState
                    size="compact"
                    variant="not-set-up"
                    icon={Server}
                    title="No worker nodes joined"
                    requirement="Join a worker before selecting a remote builder."
                    example="Build an application on a worker while the control plane handles traffic."
                    settingsHref="/settings/nodes"
                    settingsLabel="Add a worker node"
                  />
                ) : props.rosterKnown ? (
                  <Field
                    label="Add a worker"
                    error={errors.ids?.message}
                    description="Offline workers can be saved, but cannot run builds until active. Selecting a node does not change its labels or deployment role."
                  >
                    {(fieldProps) => (
                      <Picker
                        inputProps={fieldProps}
                        items={workers
                          .filter((node) => !ids.includes(node.id))
                          .map((node) => ({
                            value: String(node.id),
                            label: node.name,
                            keywords: [
                              String(node.id),
                              node.architecture ?? '',
                              node.status,
                            ],
                            description: `#${node.id} · ${node.architecture ?? 'Architecture unknown'} · ${node.status}`,
                            disabled: ids.length >= 100 || !canEdit || saving,
                          }))}
                        onValueChange={(value) => add(Number(value))}
                        placeholder="Search workers by name, ID or architecture…"
                        emptyMessage="No matching unselected workers."
                      />
                    )}
                  </Field>
                ) : (
                  <div className="space-y-3">
                    <Callout tone={props.rosterError ? 'warning' : 'info'}>
                      {props.rosterError
                        ? 'Worker details could not load. Your selected IDs are preserved.'
                        : 'Your account cannot list worker details. Ask an administrator for the worker IDs.'}
                      {props.canList && (
                        <Button
                          type="button"
                          variant="link"
                          onClick={props.retryRoster}
                        >
                          Retry worker list
                        </Button>
                      )}
                    </Callout>
                    <Field
                      label="Worker ID"
                      error={manualError || errors.ids?.message}
                      description="Positive worker ID, not the virtual control-plane node (0). The server validates it when you save."
                    >
                      {(fieldProps) => (
                        <Input
                          {...fieldProps}
                          name="workerId"
                          type="text"
                          inputMode="numeric"
                          value={manualId}
                          onChange={(event) => {
                            setManualId(event.target.value)
                            setManualError('')
                          }}
                        />
                      )}
                    </Field>
                    <Button
                      type="button"
                      variant="outline"
                      onClick={() => {
                        const id = Number(manualId)
                        if (
                          !/^\d+$/.test(manualId) ||
                          !Number.isInteger(id) ||
                          id < 1 ||
                          id > 2147483647 ||
                          ids.includes(id) ||
                          ids.length >= 100
                        ) {
                          setManualError(
                            'Enter a distinct positive worker ID; at most 100 workers.'
                          )
                          return
                        }
                        add(id)
                        setManualId('')
                      }}
                    >
                      Add worker ID
                    </Button>
                  </div>
                )}
                <Callout tone="warning" title="Trust and compatibility">
                  Only select workers you trust with application source and
                  permitted build-time inputs. Set deployment targets to worker
                  nodes only; importing worker-built images into the control
                  plane is not supported. Remote builds currently support one
                  target architecture; static image builds requiring artifact
                  extraction must use local automatic builds or a prebuilt
                  static bundle.
                </Callout>
              </div>
            )}
          </fieldset>
          <p className="text-sm text-muted-foreground">
            Applies to future builds only. Running applications, worker labels,
            and deployment placement are unchanged.
          </p>
        </SettingsGroup>
        {props.saveError != null && (
          <Callout tone="error" title="Builder settings were not saved">
            {buildNodeError(props.saveError)}
          </Callout>
        )}
        {saved && !isDirty && (
          <Callout tone="success">
            Builder settings saved. They apply to future builds; no agent
            restart is required.
          </Callout>
        )}
      </div>
    </Settings>
  )
}

function BuilderMetadata({
  id,
  nodes,
  rosterKnown,
}: {
  id: number
  nodes: NodeInfoResponse[]
  rosterKnown: boolean
}) {
  const node = nodes.find((node) => node.id === id)
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
      <span>#{id}</span>
      <span>{node?.architecture ?? 'Architecture unknown'}</span>
      <Status
        tone={
          node?.role === 'worker' && node.status === 'active' ? 'ok' : 'warn'
        }
        label={
          node
            ? node.role === 'worker'
              ? node.status
              : 'Not a worker'
            : rosterKnown
              ? 'Missing worker'
              : 'Details unavailable'
        }
      />
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useForm, useWatch, type UseFormReturn } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import {
  AlertCircle,
  ArrowRight,
  CheckCircle2,
  Circle,
  Container,
  Database,
  Eye,
  EyeOff,
  FolderGit2,
  Link2,
  Loader2,
  Plus,
  RefreshCw,
  Sparkles,
  Star,
  XCircle,
} from 'lucide-react'
import {
  getInstantiation,
  getProjectGroup,
  instantiateTemplate,
  retryInstantiation,
} from '@/api/client/sdk.gen'
import { getProjectGroupOptions } from '@/api/client/@tanstack/react-query.gen'
import { invalidateProjectLists } from '@/lib/project-list-cache'
import type {
  ExternalServiceInfo,
  InstantiateTemplateRequest,
  InstantiationResponse,
  ProjectGroupResponse,
  ServiceTypeRoute,
  TemplateResponse,
} from '@/api/client'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Checkbox } from '@/components/ui/checkbox'
import { Badge } from '@/components/ui/badge'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { ServiceLogo } from '@/components/ui/service-logo'
import { TemplateImage } from '@/components/templates/TemplateImage'
import { ServicePresetIcon } from '@/components/project-groups/ServicePresetIcon'
import {
  useInvalidateGroups,
  useProjectGroups,
} from '@/components/project-groups/useProjectGroups'
import { useTargetGroup } from '@/components/project-groups/useJoinGroupAfterCreate'
import {
  followInstantiation,
  InstantiationPollError,
} from '@/components/project-groups/follow-instantiation'
import { useAllServices } from '@/hooks/useAllServices'
import { useSettings } from '@/hooks/useSettings'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { ADD_SERVICE_TYPES } from '@/lib/addServiceTypes'
import { normalizeTemplateServiceType } from '@/lib/template-service-requirements'
import { groupHref, serviceCount } from '@/lib/terminology'
import { cn } from '@/lib/utils'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'
import {
  generatorDependsOnRepoName,
  resolveDeploymentUrlBase,
  type DeploymentUrlBase,
} from './envVarGenerators'
import {
  buildInstantiateRequest,
  compatibleDatabases,
  currentStepIndex,
  defaultServiceName,
  defaultServiceNames,
  failedStep,
  generatedValue,
  initialVariables,
  missingVariableIndexes,
  serviceSourceLabel,
  slugifyProjectName,
  validateMultiServiceForm,
  variableDeclarations,
  variableKey,
  type MultiServiceFormState,
  type VariableDeclaration,
} from './multi-service-template'

interface MultiServiceTemplateConfiguratorProps {
  template: TemplateResponse
  onCancel?: () => void
  onSuccess?: () => void
  className?: string
}

const formShape = z.object({
  target: z.enum(['new_project', 'existing_project']),
  projectName: z.string(),
  groupId: z.number().nullable(),
  serviceNames: z.array(
    z.object({ templateService: z.string(), name: z.string() })
  ),
  variables: z.array(
    z.object({
      service: z.string().nullable(),
      name: z.string(),
      value: z.string(),
      isSecret: z.boolean(),
    })
  ),
  databases: z.array(
    z.object({
      type: z.string(),
      mode: z.enum(['create', 'existing']),
      serviceId: z.number().nullable(),
    })
  ),
})

const SERVICE_LOGO_TYPES: readonly ServiceTypeRoute[] = [
  'mariadb',
  'mongodb',
  'postgres',
  'redis',
  's3',
  'kv',
  'blob',
  'rustfs',
  'minio',
]

function logoType(databaseType: string): ServiceTypeRoute | null {
  const normalized = normalizeTemplateServiceType(databaseType)
  return (
    SERVICE_LOGO_TYPES.find((candidate) => candidate === normalized) ?? null
  )
}

function databaseLabel(databaseType: string): string {
  const normalized = normalizeTemplateServiceType(databaseType)
  return (
    ADD_SERVICE_TYPES.find((type) => type.id === normalized)?.name ??
    databaseType
  )
}

/** HTTP status of a thrown Problem Details error, when there is one. */
function problemStatus(error: unknown): number | undefined {
  const status = extractProblemDetails(error)?.status
  return typeof status === 'number' ? status : undefined
}

type Progress =
  | { phase: 'following' }
  | { phase: 'opening' }
  | { phase: 'stopped' }
  | { phase: 'timed_out' }
  | { phase: 'lost'; message: string }

/**
 * Creates a template with several services in one server-side operation
 * (`POST /templates/{slug}/instantiate`), then follows it step by step.
 */
export function MultiServiceTemplateConfigurator({
  template,
  onCancel,
  onSuccess,
  className,
}: MultiServiceTemplateConfiguratorProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const invalidateGroups = useInvalidateGroups()
  const { groupId: urlGroupId, group: urlGroup } = useTargetGroup()
  const groupsQuery = useProjectGroups()
  const servicesQuery = useAllServices()
  const { data: platformSettings } = useSettings()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const deploymentUrlBase = useMemo(
    () =>
      resolveDeploymentUrlBase({
        previewDomain: platformSettings?.preview_domain,
        externalUrl: platformSettings?.external_url,
        proxyPort: platformSettings?.proxy_port,
      }),
    [
      platformSettings?.preview_domain,
      platformSettings?.external_url,
      platformSettings?.proxy_port,
    ]
  )
  const baseKey = `${deploymentUrlBase.scheme}://${deploymentUrlBase.host}:${deploymentUrlBase.port ?? ''}`

  const declarations = useMemo(() => variableDeclarations(template), [template])

  const schema = useMemo(
    () =>
      formShape.superRefine((value, ctx) => {
        for (const issue of validateMultiServiceForm(template, value)) {
          ctx.addIssue({
            code: 'custom',
            path: issue.path,
            message: issue.message,
          })
        }
      }),
    [template]
  )

  // Defaults are computed once; later changes (project slug, platform
  // settings) are applied by the effects below to untouched values only.
  const [defaults] = useState(() => {
    const projectSlug =
      urlGroupId === null ? slugifyProjectName(template.name) : ''
    const serviceNames = defaultServiceNames(template, projectSlug)
    const variables = initialVariables(
      template,
      serviceNames,
      deploymentUrlBase
    )
    const values: MultiServiceFormState = {
      target: urlGroupId === null ? 'new_project' : 'existing_project',
      projectName: template.name,
      groupId: urlGroupId,
      serviceNames,
      variables,
      databases: template.databases.map((type) => ({
        type,
        mode: 'create' as const,
        serviceId: null,
      })),
    }
    return values
  })

  const form = useForm<MultiServiceFormState>({
    resolver: zodResolver(schema),
    mode: 'onSubmit',
    defaultValues: defaults,
  })

  // Values we wrote ourselves, so a value the user typed is never replaced.
  const autoServiceNames = useRef<Record<string, string>>(
    Object.fromEntries(
      defaults.serviceNames.map((field) => [field.templateService, field.name])
    )
  )
  const autoVariables = useRef<Record<string, string>>(
    Object.fromEntries(
      defaults.variables
        .map((variable, index) => [variable, declarations[index]] as const)
        .filter(
          ([variable, declaration]) =>
            variable.value && !declaration.spec.default
        )
        .map(([variable]) => [
          variableKey(variable.service, variable.name),
          variable.value,
        ])
    )
  )

  const target = useWatch({ control: form.control, name: 'target' })
  const projectName = useWatch({ control: form.control, name: 'projectName' })
  const groupId = useWatch({ control: form.control, name: 'groupId' })
  const serviceNames = useWatch({ control: form.control, name: 'serviceNames' })
  const variables = useWatch({ control: form.control, name: 'variables' })
  const databases = useWatch({ control: form.control, name: 'databases' })

  const groups = groupsQuery.groups
  const selectableGroups = useMemo(() => {
    if (!urlGroup || groups.some((group) => group.id === urlGroup.id)) {
      return groups
    }
    return [urlGroup, ...groups]
  }, [groups, urlGroup])
  const selectedGroup: ProjectGroupResponse | null =
    selectableGroups.find((group) => group.id === groupId) ?? null

  const projectSlug =
    target === 'new_project'
      ? slugifyProjectName(projectName ?? '')
      : (selectedGroup?.slug ?? '')

  // Service names follow the project's slug until the user edits them.
  useEffect(() => {
    if (!projectSlug) return
    const current = form.getValues('serviceNames')
    current.forEach((field, index) => {
      const lastAuto = autoServiceNames.current[field.templateService]
      if (field.name !== '' && field.name !== lastAuto) return
      const next = defaultServiceName(projectSlug, field.templateService)
      if (next === field.name) return
      form.setValue(`serviceNames.${index}.name`, next, {
        shouldValidate: form.formState.isSubmitted,
      })
      autoServiceNames.current[field.templateService] = next
    })
  }, [projectSlug, form])

  // URL variables follow the service names and the platform's domain.
  const serviceNamesKey = (serviceNames ?? []).map((f) => f.name).join('\u0000')
  useEffect(() => {
    const names = form.getValues('serviceNames')
    const current = form.getValues('variables')
    current.forEach((variable, index) => {
      const declaration = declarations[index]
      if (!declaration) return
      if (!generatorDependsOnRepoName(declaration.spec.default_generator))
        return
      if (declaration.spec.default) return
      const key = variableKey(variable.service, variable.name)
      const lastAuto = autoVariables.current[key]
      if (variable.value !== '' && variable.value !== lastAuto) return
      const next = generatedValue(
        template,
        declaration,
        names,
        deploymentUrlBase
      )
      if (!next || next === variable.value) return
      form.setValue(`variables.${index}.value`, next, {
        shouldValidate: form.formState.isSubmitted,
      })
      autoVariables.current[key] = next
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serviceNamesKey, baseKey])

  const [showSecrets, setShowSecrets] = useState<Record<number, boolean>>({})
  const [submitError, setSubmitError] = useState<string | null>(null)
  const [run, setRun] = useState<InstantiationResponse | null>(null)
  const [progress, setProgress] = useState<Progress>({ phase: 'following' })

  // One key per submit attempt: a double click or a resend after a lost
  // response returns the same instantiation instead of creating it twice.
  const idempotencyKey = useRef<string | null>(null)

  const following = useRef<AbortController | null>(null)
  useEffect(() => () => following.current?.abort(), [])

  const runGroupId = run?.group_id ?? null
  const runGroupQuery = useQuery({
    ...getProjectGroupOptions({ path: { group_id: runGroupId ?? 0 } }),
    enabled: runGroupId !== null,
    retry: false,
  })
  const runGroup = runGroupQuery.data ?? null

  const invalidateAll = useCallback(async () => {
    await Promise.all([
      invalidateGroups(),
      invalidateProjectLists(queryClient),
      queryClient.invalidateQueries({ queryKey: ['external-services'] }),
    ])
  }, [invalidateGroups, queryClient])

  /** Where the created services are: the project, else the first service. */
  const destinationOf = useCallback(
    async (instantiation: InstantiationResponse): Promise<string> => {
      const firstService = instantiation.services.find(
        (service) => service.project_id != null
      )
      const fallback = firstService
        ? `/projects/${firstService.slug}`
        : '/projects'
      if (instantiation.group_id == null) return fallback
      try {
        const { data } = await getProjectGroup({
          path: { group_id: instantiation.group_id },
          throwOnError: true,
        })
        return groupHref(data.slug)
      } catch {
        return fallback
      }
    },
    []
  )

  const follow = useCallback(
    async (id: number) => {
      following.current?.abort()
      const controller = new AbortController()
      following.current = controller
      setProgress({ phase: 'following' })
      try {
        const outcome = await followInstantiation(id, {
          signal: controller.signal,
          fetchInstantiation: async (instantiationId, signal) =>
            (
              await getInstantiation({
                path: { id: instantiationId },
                signal,
                throwOnError: true,
              })
            ).data,
          onUpdate: setRun,
        })
        if (outcome.kind === 'aborted') return
        await invalidateAll()
        if (outcome.kind === 'timed_out') {
          if (outcome.instantiation) setRun(outcome.instantiation)
          setProgress({ phase: 'timed_out' })
          return
        }
        const result = outcome.instantiation
        setRun(result)
        if (result.status !== 'succeeded') {
          setProgress({ phase: 'stopped' })
          return
        }
        setProgress({ phase: 'opening' })
        toast.success(
          `${template.name} is ready: ${serviceCount(result.services.length)} created`
        )
        onSuccess?.()
        navigate(await destinationOf(result))
      } catch (error) {
        if (controller.signal.aborted) return
        setProgress({
          phase: 'lost',
          message:
            error instanceof InstantiationPollError
              ? error.message
              : getErrorMessage(
                  error,
                  `Lost track of #${id}. It may still be running on the server.`
                ),
        })
      }
    },
    [destinationOf, invalidateAll, navigate, onSuccess, template.name]
  )

  const instantiate = useMutation({
    mutationFn: async (body: InstantiateTemplateRequest) =>
      (
        await instantiateTemplate({
          path: { slug: template.slug },
          body,
          throwOnError: true,
        })
      ).data,
    onSuccess: (data) => {
      setSubmitError(null)
      setRun(data)
      void follow(data.id)
    },
    onError: (error, body) => {
      // ADR 045: a reserved slug needs a recently verified session; the
      // same body (and key) is sent again once the admin re-verifies.
      if (handleSensitiveActionError(error, () => instantiate.mutate(body))) {
        return
      }
      // A definite rejection stored nothing, so the next attempt can use a
      // fresh key. A conflict (the key is taken) or a lost response keeps it.
      const status = problemStatus(error)
      if (
        status !== undefined &&
        status >= 400 &&
        status < 500 &&
        status !== 409
      ) {
        idempotencyKey.current = null
      }
      setSubmitError(
        getErrorMessage(error, 'The template could not be created.')
      )
    },
  })

  const retry = useMutation({
    mutationFn: async (id: number) =>
      (await retryInstantiation({ path: { id }, throwOnError: true })).data,
    onSuccess: (data) => {
      setRun(data)
      void follow(data.id)
    },
    onError: (error, id) => {
      if (handleSensitiveActionError(error, () => retry.mutate(id))) return
      toast.error(getErrorMessage(error, `#${id} could not be retried.`))
    },
  })

  const handleSubmit = (values: MultiServiceFormState) => {
    if (!idempotencyKey.current) idempotencyKey.current = crypto.randomUUID()
    setSubmitError(null)
    instantiate.mutate(buildInstantiateRequest(values, idempotencyKey.current))
  }

  // Built when the form is submitted, not while rendering: `handleSubmit`
  // reads the idempotency-key ref, which must not happen during render.
  const submitForm = (event: React.BaseSyntheticEvent) =>
    form.handleSubmit(handleSubmit, () =>
      toast.error('Review the highlighted fields before creating the services.')
    )(event)

  const keepInBackground = () => {
    following.current?.abort()
    toast.info(
      `${template.name} keeps being created on the server. Its progress is shown on the project page.`
    )
    navigate(runGroup ? groupHref(runGroup.slug) : '/projects')
  }

  const missingIndexes = useMemo(
    () => missingVariableIndexes(template, variables ?? []),
    [template, variables]
  )

  const header = <TemplateHeader template={template} />

  if (run) {
    return (
      <div className={cn('space-y-6', className)}>
        {verificationDialog}
        {header}
        <InstantiationProgress
          template={template}
          run={run}
          progress={progress}
          group={runGroup}
          retrying={retry.isPending}
          onRetry={() => retry.mutate(run.id)}
          onFollowAgain={() => void follow(run.id)}
          onKeepInBackground={keepInBackground}
        />
      </div>
    )
  }

  const sectionFor = (service: string | null) =>
    declarations
      .map((declaration, index) => ({ declaration, index }))
      .filter(({ declaration }) => declaration.service === service)

  return (
    <div className={cn('space-y-6', className)}>
      {verificationDialog}
      {header}

      <Form {...form}>
        <form onSubmit={submitForm} className="space-y-6">
          {/* Target project */}
          <Card>
            <CardHeader>
              <CardTitle>Project</CardTitle>
              <CardDescription>
                The {serviceCount(template.service_definitions.length)} of this
                template share routes and variables, so they go into one
                project.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <FormField
                control={form.control}
                name="target"
                render={({ field }) => (
                  <FormItem>
                    <FormControl>
                      <RadioGroup
                        value={field.value}
                        onValueChange={(value) => field.onChange(value)}
                        className="grid grid-cols-1 gap-3 md:grid-cols-2"
                      >
                        <TargetCard
                          id="target-new"
                          value="new_project"
                          checked={field.value === 'new_project'}
                          icon={<Plus className="h-4 w-4" />}
                          title="New project"
                          description="Create a project for these services"
                        />
                        <TargetCard
                          id="target-existing"
                          value="existing_project"
                          checked={field.value === 'existing_project'}
                          icon={<FolderGit2 className="h-4 w-4" />}
                          title="Existing project"
                          description="Add the services to a project you have"
                        />
                      </RadioGroup>
                    </FormControl>
                  </FormItem>
                )}
              />

              {target === 'new_project' ? (
                <FormField
                  control={form.control}
                  name="projectName"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Project name</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="My project" />
                      </FormControl>
                      {projectSlug && (
                        <FormDescription>
                          Project address:{' '}
                          <code className="rounded bg-muted px-1 py-0.5 text-xs">
                            {groupHref(projectSlug)}
                          </code>
                        </FormDescription>
                      )}
                      <FormMessage />
                    </FormItem>
                  )}
                />
              ) : (
                <FormField
                  control={form.control}
                  name="groupId"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Choose a project</FormLabel>
                      <GroupPicker
                        groups={selectableGroups}
                        isLoading={groupsQuery.isLoading}
                        isError={groupsQuery.isError}
                        error={groupsQuery.error}
                        onRetry={() => void groupsQuery.refetch()}
                        value={field.value}
                        onChange={field.onChange}
                        onCreateNew={() =>
                          form.setValue('target', 'new_project')
                        }
                      />
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
            </CardContent>
          </Card>

          {/* Services preview */}
          <Card>
            <CardHeader>
              <CardTitle>Services</CardTitle>
              <CardDescription>
                {template.service_definitions.some((service) => !service.image)
                  ? `Services that build from source deploy from the template's public repository${template.git?.url ? ` (${template.git.url})` : ''}.`
                  : 'Each service runs a prebuilt image.'}
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
                {template.service_definitions.map((service, index) => (
                  <div
                    key={service.name}
                    className="space-y-3 rounded-lg border p-4"
                  >
                    <div className="flex items-start gap-3">
                      <ServicePresetIcon
                        preset={service.preset}
                        className="size-8"
                      />
                      <div className="min-w-0 flex-1 space-y-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-sm font-medium">
                            {service.name}
                          </span>
                          {service.exposed_port != null && (
                            <Badge variant="outline" className="text-xs">
                              Port {service.exposed_port}
                            </Badge>
                          )}
                        </div>
                        <p className="flex items-center gap-1.5 break-all text-xs text-muted-foreground">
                          {service.image ? (
                            <Container className="h-3.5 w-3.5 shrink-0" />
                          ) : (
                            <FolderGit2 className="h-3.5 w-3.5 shrink-0" />
                          )}
                          {serviceSourceLabel(service)}
                        </p>
                        {service.databases.length > 0 && (
                          <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
                            <Database className="h-3.5 w-3.5 shrink-0" />
                            Uses{' '}
                            {service.databases.map(databaseLabel).join(', ')}
                          </p>
                        )}
                      </div>
                    </div>
                    <FormField
                      control={form.control}
                      name={`serviceNames.${index}.name`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-xs">
                            Service name
                          </FormLabel>
                          <FormControl>
                            <Input
                              {...field}
                              className="font-mono text-sm"
                              autoCapitalize="none"
                              autoCorrect="off"
                              spellCheck={false}
                            />
                          </FormControl>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>
                ))}
              </div>

              {template.routes.length > 0 && (
                <div className="space-y-2 rounded-lg border bg-muted/30 p-4">
                  <p className="text-sm font-medium">Routes</p>
                  <ul className="space-y-1.5">
                    {template.routes.map((route) => (
                      <li
                        key={`${route.source}:${route.prefix}`}
                        className="flex flex-wrap items-center gap-2 text-sm"
                      >
                        <code className="rounded bg-background px-1.5 py-0.5 text-xs">
                          {route.prefix}
                        </code>
                        <span className="text-xs text-muted-foreground">
                          on {route.source}
                        </span>
                        <ArrowRight className="h-3.5 w-3.5 text-muted-foreground" />
                        <span className="font-medium">{route.target}</span>
                        {route.strip_prefix && (
                          <span className="text-xs text-muted-foreground">
                            (prefix removed)
                          </span>
                        )}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Databases */}
          {databases.length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Databases</CardTitle>
                <CardDescription>
                  Create a new managed database or link one you have. Its
                  connection variables are injected into the services that use
                  it.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-4">
                {databases.map((database, index) => (
                  <DatabaseChoiceField
                    key={database.type}
                    index={index}
                    databaseType={database.type}
                    usedBy={template.service_definitions
                      .filter((service) =>
                        service.databases.some(
                          (type) =>
                            normalizeTemplateServiceType(type) ===
                            normalizeTemplateServiceType(database.type)
                        )
                      )
                      .map((service) => service.name)}
                    form={form}
                    services={servicesQuery.data ?? []}
                    isLoading={servicesQuery.isPending}
                    isError={servicesQuery.isError}
                    onRetry={() => void servicesQuery.refetch()}
                  />
                ))}
              </CardContent>
            </Card>
          )}

          {/* Variables */}
          {declarations.length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Environment variables</CardTitle>
                <CardDescription>
                  All values are encrypted at rest. Required values are marked
                  with *; generated secrets are filled in for you. You can add
                  more variables after the services are created.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-6">
                {template.service_definitions.map((service) => {
                  const rows = sectionFor(service.name)
                  if (rows.length === 0) return null
                  return (
                    <VariableSection
                      key={service.name}
                      title={service.name}
                      icon={
                        <ServicePresetIcon
                          preset={service.preset}
                          className="size-5"
                        />
                      }
                      rows={rows}
                      form={form}
                      template={template}
                      deploymentUrlBase={deploymentUrlBase}
                      showSecrets={showSecrets}
                      onToggleSecret={(index) =>
                        setShowSecrets((prev) => ({
                          ...prev,
                          [index]: !prev[index],
                        }))
                      }
                      onGenerated={(key, value) => {
                        autoVariables.current[key] = value
                      }}
                    />
                  )
                })}
                {sectionFor(null).length > 0 && (
                  <VariableSection
                    title="Shared by all services"
                    description="Saved as project variables, so every service of the project receives them. An existing project keeps the values it already defines."
                    icon={<Link2 className="h-4 w-4 text-muted-foreground" />}
                    rows={sectionFor(null)}
                    form={form}
                    template={template}
                    deploymentUrlBase={deploymentUrlBase}
                    showSecrets={showSecrets}
                    onToggleSecret={(index) =>
                      setShowSecrets((prev) => ({
                        ...prev,
                        [index]: !prev[index],
                      }))
                    }
                    onGenerated={(key, value) => {
                      autoVariables.current[key] = value
                    }}
                  />
                )}
              </CardContent>
            </Card>
          )}

          {submitError && (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertTitle>The services were not created</AlertTitle>
              <AlertDescription>{submitError}</AlertDescription>
            </Alert>
          )}

          {missingIndexes.length > 0 && (
            <Alert variant="destructive">
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Missing required values:{' '}
                {missingIndexes
                  .map((index) => {
                    const variable = (variables ?? [])[index]
                    return variable.service
                      ? `${variable.name} (${variable.service})`
                      : variable.name
                  })
                  .join(', ')}
              </AlertDescription>
            </Alert>
          )}

          <div className="flex flex-col-reverse gap-3 sm:flex-row sm:items-center sm:justify-end">
            {target === 'existing_project' && selectedGroup && (
              <p className="text-sm text-muted-foreground sm:mr-auto">
                These services will be added to {selectedGroup.name}.
              </p>
            )}
            {onCancel && (
              <Button
                type="button"
                variant="outline"
                onClick={onCancel}
                disabled={instantiate.isPending}
              >
                Cancel
              </Button>
            )}
            <Button
              type="submit"
              disabled={instantiate.isPending || missingIndexes.length > 0}
            >
              {instantiate.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <CheckCircle2 className="mr-2 h-4 w-4" />
              )}
              Create {serviceCount(template.service_definitions.length)}
            </Button>
          </div>
        </form>
      </Form>
    </div>
  )
}

function TemplateHeader({ template }: { template: TemplateResponse }) {
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex items-center gap-3">
            <TemplateImage
              imageUrl={template.image_url}
              preset={template.preset}
              alt={template.name}
              className="h-12 w-12"
              imgClassName="h-10 w-10"
              fallbackClassName="h-6 w-6"
            />
            <div>
              <CardTitle className="flex items-center gap-2 text-lg">
                {template.name}
                {template.is_featured && (
                  <Star className="h-4 w-4 fill-yellow-500 text-yellow-500" />
                )}
              </CardTitle>
              <CardDescription>{template.description}</CardDescription>
            </div>
          </div>
          <Badge variant="secondary" className="self-start">
            {serviceCount(template.service_definitions.length)}
          </Badge>
        </div>
      </CardHeader>
      {(template.tags.length > 0 || template.features.length > 0) && (
        <CardContent>
          <div className="flex flex-wrap gap-2">
            {template.tags.map((tag) => (
              <Badge key={tag} variant="outline" className="text-xs">
                {tag}
              </Badge>
            ))}
          </div>
          {template.features.length > 0 && (
            <div className="mt-3 text-sm text-muted-foreground">
              <strong className="text-foreground">Features:</strong>{' '}
              {template.features.join(' · ')}
            </div>
          )}
        </CardContent>
      )}
    </Card>
  )
}

function TargetCard({
  id,
  value,
  checked,
  icon,
  title,
  description,
}: {
  id: string
  value: string
  checked: boolean
  icon: React.ReactNode
  title: string
  description: string
}) {
  return (
    <Label
      htmlFor={id}
      className={cn(
        'flex cursor-pointer items-start gap-3 rounded-lg border p-4 transition-colors hover:bg-accent/50',
        checked && 'border-primary bg-accent/50'
      )}
    >
      <RadioGroupItem id={id} value={value} className="mt-0.5" />
      <span className="mt-0.5 text-muted-foreground">{icon}</span>
      <span className="space-y-1">
        <span className="block text-sm font-medium">{title}</span>
        <span className="block text-xs font-normal text-muted-foreground">
          {description}
        </span>
      </span>
    </Label>
  )
}

function GroupPicker({
  groups,
  isLoading,
  isError,
  error,
  onRetry,
  value,
  onChange,
  onCreateNew,
}: {
  groups: ProjectGroupResponse[]
  isLoading: boolean
  isError: boolean
  error: unknown
  onRetry: () => void
  value: number | null
  onChange: (id: number) => void
  onCreateNew: () => void
}) {
  if (isLoading) {
    return (
      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-16 w-full" />
      </div>
    )
  }
  if (isError) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertDescription className="flex flex-wrap items-center justify-between gap-3">
          <span>
            {getErrorMessage(error, 'Your projects could not be loaded.')}
          </span>
          <Button type="button" variant="outline" size="sm" onClick={onRetry}>
            <RefreshCw className="mr-2 h-4 w-4" />
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  }
  if (groups.length === 0) {
    return (
      <div className="flex flex-col gap-3 rounded-lg border border-dashed p-4 text-sm sm:flex-row sm:items-center sm:justify-between">
        <span className="text-muted-foreground">You have no projects yet.</span>
        <Button type="button" variant="outline" size="sm" onClick={onCreateNew}>
          <Plus className="mr-2 h-4 w-4" />
          Create a new project
        </Button>
      </div>
    )
  }
  return (
    <RadioGroup
      value={value === null ? '' : String(value)}
      onValueChange={(next) => onChange(Number(next))}
      className="grid max-h-80 grid-cols-1 gap-2 overflow-y-auto md:grid-cols-2"
    >
      {groups.map((group) => {
        const id = `target-group-${group.id}`
        const checked = value === group.id
        return (
          <Label
            key={group.id}
            htmlFor={id}
            className={cn(
              'flex cursor-pointer items-center gap-3 rounded-md border p-3 transition-colors hover:bg-accent/50',
              checked && 'border-primary bg-accent/50'
            )}
          >
            <RadioGroupItem id={id} value={String(group.id)} />
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium">
                {group.name}
              </span>
              <span className="block text-xs font-normal text-muted-foreground">
                {serviceCount(group.projects.length)}
              </span>
            </span>
          </Label>
        )
      })}
    </RadioGroup>
  )
}

type MultiServiceForm = UseFormReturn<MultiServiceFormState>

function DatabaseChoiceField({
  index,
  databaseType,
  usedBy,
  form,
  services,
  isLoading,
  isError,
  onRetry,
}: {
  index: number
  databaseType: string
  usedBy: string[]
  form: MultiServiceForm
  services: ExternalServiceInfo[]
  isLoading: boolean
  isError: boolean
  onRetry: () => void
}) {
  const mode = useWatch({
    control: form.control,
    name: `databases.${index}.mode`,
  })
  const label = databaseLabel(databaseType)
  const logo = logoType(databaseType)
  const compatible = compatibleDatabases(databaseType, services)

  return (
    <div className="space-y-3 rounded-lg border p-4">
      <div className="flex flex-wrap items-center gap-2">
        {logo ? (
          <ServiceLogo service={logo} className="h-6 w-6" />
        ) : (
          <Database className="h-6 w-6 text-muted-foreground" />
        )}
        <span className="text-sm font-medium">{label}</span>
        {usedBy.length > 0 && (
          <span className="text-xs text-muted-foreground">
            used by {usedBy.join(', ')}
          </span>
        )}
      </div>
      <FormField
        control={form.control}
        name={`databases.${index}.mode`}
        render={({ field }) => (
          <FormItem>
            <FormControl>
              <RadioGroup
                value={field.value}
                onValueChange={(value) => {
                  field.onChange(value)
                  if (value === 'create') {
                    form.setValue(`databases.${index}.serviceId`, null)
                  }
                }}
                className="grid grid-cols-1 gap-2 md:grid-cols-2"
              >
                <TargetCard
                  id={`database-${index}-create`}
                  value="create"
                  checked={field.value === 'create'}
                  icon={<Plus className="h-4 w-4" />}
                  title={`Create a new ${label} database`}
                  description="Temps creates it with default settings"
                />
                <TargetCard
                  id={`database-${index}-existing`}
                  value="existing"
                  checked={field.value === 'existing'}
                  icon={<Link2 className="h-4 w-4" />}
                  title="Link an existing one"
                  description={`Share a ${label} database you already have`}
                />
              </RadioGroup>
            </FormControl>
          </FormItem>
        )}
      />
      {mode === 'existing' && (
        <FormField
          control={form.control}
          name={`databases.${index}.serviceId`}
          render={({ field }) => (
            <FormItem>
              {isLoading ? (
                <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                  <Skeleton className="h-12 w-full" />
                  <Skeleton className="h-12 w-full" />
                </div>
              ) : isError ? (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription className="flex flex-wrap items-center justify-between gap-3">
                    <span>Your databases could not be loaded.</span>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={onRetry}
                    >
                      <RefreshCw className="mr-2 h-4 w-4" />
                      Retry
                    </Button>
                  </AlertDescription>
                </Alert>
              ) : compatible.length === 0 ? (
                <p className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
                  You have no {label} database yet. Choose &quot;Create a new{' '}
                  {label} database&quot; instead.
                </p>
              ) : (
                <RadioGroup
                  value={field.value === null ? '' : String(field.value)}
                  onValueChange={(next) => field.onChange(Number(next))}
                  className="grid grid-cols-1 gap-2 md:grid-cols-2"
                >
                  {compatible.map((service) => {
                    const id = `database-${index}-service-${service.id}`
                    const checked = field.value === service.id
                    return (
                      <Label
                        key={service.id}
                        htmlFor={id}
                        className={cn(
                          'flex cursor-pointer items-center gap-3 rounded-md border p-3 transition-colors hover:bg-accent/50',
                          checked && 'border-primary bg-accent/50'
                        )}
                      >
                        <RadioGroupItem id={id} value={String(service.id)} />
                        <ServiceLogo
                          service={service.service_type}
                          className="h-5 w-5"
                        />
                        <span className="truncate text-sm font-medium">
                          {service.name}
                        </span>
                      </Label>
                    )
                  })}
                </RadioGroup>
              )}
              <FormMessage />
            </FormItem>
          )}
        />
      )}
    </div>
  )
}

function VariableSection({
  title,
  description,
  icon,
  rows,
  form,
  template,
  deploymentUrlBase,
  showSecrets,
  onToggleSecret,
  onGenerated,
}: {
  title: string
  description?: string
  icon: React.ReactNode
  rows: {
    declaration: VariableDeclaration
    index: number
  }[]
  form: MultiServiceForm
  template: TemplateResponse
  deploymentUrlBase: DeploymentUrlBase
  showSecrets: Record<number, boolean>
  onToggleSecret: (index: number) => void
  onGenerated: (key: string, value: string) => void
}) {
  const variables = useWatch({ control: form.control, name: 'variables' })
  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          {icon}
          <p className="text-sm font-medium">{title}</p>
        </div>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>
      <div className="divide-y divide-border/60 overflow-hidden rounded-lg border border-border/60">
        {rows.map(({ declaration, index }) => {
          const { spec } = declaration
          const isSecret = variables?.[index]?.isSecret ?? false
          const generator = spec.default_generator
          const handleGenerate = () => {
            const value = generatedValue(
              template,
              declaration,
              form.getValues('serviceNames'),
              deploymentUrlBase
            )
            if (!value) {
              toast.error(
                generator === 'app_url'
                  ? 'Enter a service name first'
                  : 'Could not generate a value'
              )
              return
            }
            form.setValue(`variables.${index}.value`, value, {
              shouldValidate: form.formState.isSubmitted,
            })
            onGenerated(variableKey(declaration.service, spec.name), value)
          }
          return (
            <div
              key={variableKey(declaration.service, spec.name)}
              className="grid gap-3 px-4 py-3 lg:grid-cols-[minmax(0,5fr)_minmax(0,7fr)_auto] lg:items-start lg:gap-4"
            >
              <div className="min-w-0 space-y-1">
                <p className="break-all font-mono text-sm">
                  {spec.name}
                  {spec.required && (
                    <span
                      className="ml-1 text-muted-foreground"
                      title="Required"
                    >
                      <span aria-hidden="true">*</span>
                      <span className="sr-only">(required)</span>
                    </span>
                  )}
                </p>
                {spec.description && (
                  <p className="text-pretty text-xs text-muted-foreground">
                    {spec.description}
                  </p>
                )}
              </div>
              <FormField
                control={form.control}
                name={`variables.${index}.value`}
                render={({ field }) => (
                  <FormItem className="min-w-0 space-y-1.5">
                    <FormLabel className="sr-only">
                      Value of {spec.name}
                    </FormLabel>
                    <div className="relative">
                      <FormControl>
                        <Input
                          {...field}
                          type={
                            isSecret && !showSecrets[index]
                              ? 'password'
                              : 'text'
                          }
                          placeholder={spec.example || 'Enter value'}
                          autoCapitalize="none"
                          autoCorrect="off"
                          spellCheck={false}
                          className={cn(
                            'font-mono',
                            generator && isSecret && 'pr-20',
                            generator && !isSecret && 'pr-10',
                            !generator && isSecret && 'pr-10'
                          )}
                        />
                      </FormControl>
                      {generator && (
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className={cn(
                            'absolute top-0 h-full px-2',
                            isSecret ? 'right-9' : 'right-0'
                          )}
                          onClick={handleGenerate}
                          title={
                            generator === 'app_url'
                              ? 'Generate from the service name'
                              : 'Generate a random value'
                          }
                          aria-label={`Generate a value for ${spec.name}`}
                        >
                          <Sparkles className="h-4 w-4" />
                        </Button>
                      )}
                      {isSecret && (
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="absolute right-0 top-0 h-full px-3"
                          onClick={() => onToggleSecret(index)}
                          aria-label={
                            showSecrets[index]
                              ? 'Hide secret value'
                              : 'Show secret value'
                          }
                        >
                          {showSecrets[index] ? (
                            <EyeOff className="h-4 w-4" />
                          ) : (
                            <Eye className="h-4 w-4" />
                          )}
                        </Button>
                      )}
                    </div>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name={`variables.${index}.isSecret`}
                render={({ field }) => (
                  <FormItem className="flex min-h-10 items-center gap-2 space-y-0 lg:w-24">
                    <FormControl>
                      <Checkbox
                        checked={field.value}
                        onCheckedChange={(checked) =>
                          field.onChange(checked === true)
                        }
                      />
                    </FormControl>
                    <FormLabel className="text-sm font-normal">
                      Secret
                    </FormLabel>
                  </FormItem>
                )}
              />
            </div>
          )
        })}
      </div>
    </div>
  )
}

function StepIcon({
  status,
  current,
}: {
  status: InstantiationResponse['steps'][number]['status']
  current: boolean
}) {
  if (status === 'done') {
    return (
      <CheckCircle2 className="h-4 w-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
    )
  }
  if (status === 'failed') {
    return <XCircle className="h-4 w-4 shrink-0 text-destructive" />
  }
  if (current) {
    return <Loader2 className="h-4 w-4 shrink-0 animate-spin text-primary" />
  }
  return <Circle className="h-4 w-4 shrink-0 text-muted-foreground/60" />
}

function InstantiationProgress({
  template,
  run,
  progress,
  group,
  retrying,
  onRetry,
  onFollowAgain,
  onKeepInBackground,
}: {
  template: TemplateResponse
  run: InstantiationResponse
  progress: Progress
  group: ProjectGroupResponse | null
  retrying: boolean
  onRetry: () => void
  onFollowAgain: () => void
  onKeepInBackground: () => void
}) {
  const current = currentStepIndex(run)
  const failed = failedStep(run)
  const stopped =
    progress.phase === 'stopped' &&
    (run.status === 'failed' || run.status === 'interrupted')
  const projectLink = group ? (
    <Button type="button" variant="outline" asChild>
      <Link to={groupHref(group.slug)}>Open {group.name}</Link>
    </Button>
  ) : null

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {progress.phase === 'opening'
            ? `${template.name} is ready`
            : stopped
              ? `Creating ${template.name} stopped`
              : `Creating ${template.name}`}
        </CardTitle>
        <CardDescription>
          #{run.id} · {serviceCount(run.services.length)}
          {group ? ` in ${group.name}` : ''}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ol className="space-y-2">
          {run.steps.map((step, index) => (
            <li key={step.key} className="space-y-1">
              <div className="flex items-center gap-2 text-sm">
                <StepIcon
                  status={step.status}
                  current={index === current && progress.phase === 'following'}
                />
                <span
                  className={cn(
                    step.status === 'pending' &&
                      index !== current &&
                      'text-muted-foreground'
                  )}
                >
                  {step.label}
                </span>
              </div>
              {step.error && (
                <p className="ml-6 break-words text-xs text-destructive">
                  {step.error}
                </p>
              )}
            </li>
          ))}
        </ol>

        {progress.phase === 'opening' && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            Opening the project…
          </p>
        )}

        {stopped && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>
              {run.status === 'interrupted'
                ? 'The server restarted before it finished'
                : failed
                  ? `"${failed.label}" failed`
                  : 'A step failed'}
            </AlertTitle>
            <AlertDescription className="space-y-1">
              {run.status !== 'interrupted' && (
                <p>{failed?.error ?? run.error ?? 'No error was reported.'}</p>
              )}
              <p>
                Retrying continues from the step that stopped; what was already
                created is kept.
                {group
                  ? ' It is also listed on the project page, where you can retry later.'
                  : ''}
              </p>
            </AlertDescription>
          </Alert>
        )}

        {progress.phase === 'timed_out' && (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Still running</AlertTitle>
            <AlertDescription>
              #{run.id} is taking longer than expected and keeps running on the
              server.{' '}
              {group
                ? 'Its progress is shown on the project page.'
                : 'Its services appear in your service list once created.'}
            </AlertDescription>
          </Alert>
        )}

        {progress.phase === 'lost' && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Progress could not be loaded</AlertTitle>
            <AlertDescription>{progress.message}</AlertDescription>
          </Alert>
        )}

        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
          {progress.phase === 'following' && (
            <Button
              type="button"
              variant="outline"
              onClick={onKeepInBackground}
            >
              Keep running in background
            </Button>
          )}
          {(stopped ||
            progress.phase === 'timed_out' ||
            progress.phase === 'lost') &&
            (projectLink ?? (
              <Button type="button" variant="outline" asChild>
                <Link to="/projects">Open services</Link>
              </Button>
            ))}
          {stopped && (
            <Button type="button" onClick={onRetry} disabled={retrying}>
              {retrying ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <RefreshCw className="mr-2 h-4 w-4" />
              )}
              Retry
            </Button>
          )}
          {(progress.phase === 'timed_out' || progress.phase === 'lost') && (
            <Button type="button" onClick={onFollowAgain}>
              <RefreshCw className="mr-2 h-4 w-4" />
              Check again
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

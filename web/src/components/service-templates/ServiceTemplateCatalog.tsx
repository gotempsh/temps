// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createProject,
  deployFromUploadedSource,
  getEnvironments,
  preflightServiceTemplate,
} from '@/api/client'
import {
  getServiceTemplateOptions,
  listServiceTemplatesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ServiceTemplateDetailResponse,
  ServiceTemplateSummaryResponse,
} from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { prepareDrop } from '@/lib/drop-archive'
import {
  createServiceTemplateWithSlugRetry,
  generateDependentServiceTemplateValue,
  generateServiceTemplateValue,
  serviceTemplateVariableIsGenerated,
} from '@/lib/service-template-values'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'
import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowLeft,
  Box,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  ExternalLink,
  Eye,
  EyeOff,
  Loader2,
  RefreshCw,
  Search,
  ShieldAlert,
  Sparkles,
} from 'lucide-react'
import { useDeferredValue, useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'

const PER_PAGE = 24

function templateLogo(template: ServiceTemplateSummaryResponse) {
  if (!template.logo_url) {
    return (
      <div className="flex size-12 items-center justify-center rounded-xl bg-muted">
        <Box className="size-6 text-muted-foreground" />
      </div>
    )
  }
  return (
    <div className="flex size-12 items-center justify-center rounded-xl border bg-white p-2 dark:bg-white/90">
      <img
        src={template.logo_url}
        alt=""
        className="max-h-full max-w-full object-contain"
        loading="lazy"
      />
    </div>
  )
}

function CatalogError({ error, retry }: { error: unknown; retry: () => void }) {
  return (
    <Alert variant="destructive">
      <AlertTriangle className="size-4" />
      <AlertTitle>Service catalog is unavailable</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>
          {getErrorMessage(error, 'Temps could not load the Coolify catalog.')}
        </p>
        <p>
          Confirm this Temps host can reach <code>cdn.coollabs.io</code>. The
          Services page stays available and can be retried after outbound access
          is restored.
        </p>
        <Button type="button" variant="outline" size="sm" onClick={retry}>
          <RefreshCw className="mr-2 size-4" />
          Retry
        </Button>
      </AlertDescription>
    </Alert>
  )
}

function TemplateCard({
  template,
  onSelect,
}: {
  template: ServiceTemplateSummaryResponse
  onSelect: () => void
}) {
  return (
    <Card className="group flex h-full flex-col transition-colors hover:border-foreground/25">
      <CardHeader className="space-y-4">
        <div className="flex items-start justify-between gap-3">
          {templateLogo(template)}
          {template.compatibility_tier === 'standard' ? (
            <Badge
              variant="secondary"
              className="border-emerald-500/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
            >
              <CheckCircle2 className="mr-1 size-3" />
              Ready
            </Badge>
          ) : template.compatibility_tier === 'elevated' ? (
            <Badge className="border-amber-500/20 bg-amber-500/10 text-amber-700 dark:text-amber-300">
              <ShieldAlert className="mr-1 size-3" />
              Approval required
            </Badge>
          ) : (
            <Badge variant="outline" className="text-muted-foreground">
              <ShieldAlert className="mr-1 size-3" />
              Blocked
            </Badge>
          )}
        </div>
        <div>
          <CardTitle className="text-base">{template.name}</CardTitle>
          <CardDescription className="mt-1 line-clamp-3 min-h-15">
            {template.description || 'A self-hosted Docker Compose service.'}
          </CardDescription>
        </div>
      </CardHeader>
      <CardContent className="mt-auto space-y-4">
        <div className="flex flex-wrap gap-1.5">
          <Badge variant="outline">{template.category}</Badge>
          <Badge variant="outline">
            {template.service_count}{' '}
            {template.service_count === 1 ? 'container' : 'containers'}
          </Badge>
          {template.amd_only && <Badge variant="outline">AMD64 only</Badge>}
          {template.arm_only && <Badge variant="outline">ARM64 only</Badge>}
        </div>
        <Button
          type="button"
          variant={template.installable ? 'default' : 'outline'}
          className="w-full"
          onClick={onSelect}
        >
          {template.installable ? 'Configure service' : 'Review compatibility'}
        </Button>
      </CardContent>
    </Card>
  )
}

function TemplateInstaller({
  detail,
  onBack,
}: {
  detail: ServiceTemplateDetailResponse
  onBack: () => void
}) {
  const navigate = useNavigate()
  const [name, setName] = useState(detail.name)
  const [visible, setVisible] = useState<Record<string, boolean>>({})
  const [installing, setInstalling] = useState(false)
  const [approvedCapabilities, setApprovedCapabilities] = useState<
    Record<string, boolean>
  >({})
  const [preflightErrors, setPreflightErrors] = useState<string[]>([])
  const [generationError, setGenerationError] = useState<string | null>(null)
  const [generatingDependencies, setGeneratingDependencies] = useState(false)
  const routeForVariable = (
    variable: ServiceTemplateDetailResponse['variables'][number]
  ) =>
    detail.routes.find((route) =>
      route.variable_names.includes(variable.name)
    ) || detail.routes.find((route) => route.service === variable.route_service)
  const generatorInput = (
    variable: ServiceTemplateDetailResponse['variables'][number]
  ) => {
    const route = routeForVariable(variable)
    return {
      name: variable.name,
      kind: variable.kind,
      defaultValue: variable.default_value,
      routeService: variable.route_service,
      routeIsPrimary:
        !route ||
        detail.routes.findIndex((candidate) => candidate === route) === 0,
    }
  }
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(
      detail.variables
        .filter(
          (variable) =>
            variable.kind !== 'public_url' && variable.kind !== 'public_host'
        )
        .map((variable) => [
          variable.name,
          generateServiceTemplateValue(generatorInput(variable), detail.slug),
        ])
    )
  )
  const jwtSigningKey = values.SERVICE_PASSWORD_JWT
  useEffect(() => {
    let cancelled = false
    const dependentVariables = detail.variables.filter((variable) =>
      ['generated_supabase_anon', 'generated_supabase_service'].includes(
        variable.kind
      )
    )
    const resolveDependencies = async () => {
      // Defer state synchronization so the effect itself only coordinates the
      // asynchronous WebCrypto work and cannot trigger a cascading render.
      await Promise.resolve()
      if (cancelled) return
      setValues((current) => ({
        ...current,
        ...Object.fromEntries(
          dependentVariables.map((variable) => [variable.name, ''])
        ),
      }))
      setGenerationError(null)
      if (!jwtSigningKey) {
        setGeneratingDependencies(false)
        return
      }
      setGeneratingDependencies(true)
      try {
        const generated = await Promise.all(
          dependentVariables.map(
            async (variable) =>
              [
                variable.name,
                await generateDependentServiceTemplateValue(variable.kind, {
                  SERVICE_PASSWORD_JWT: jwtSigningKey,
                }),
              ] as const
          )
        )
        if (cancelled) return
        setValues((current) => ({
          ...current,
          ...Object.fromEntries(
            generated.filter((entry): entry is readonly [string, string] =>
              Boolean(entry[1])
            )
          ),
        }))
      } catch (error) {
        if (!cancelled) {
          setGenerationError(
            getErrorMessage(error, 'Could not generate dependent credentials')
          )
        }
      } finally {
        if (!cancelled) setGeneratingDependencies(false)
      }
    }
    void resolveDependencies()
    return () => {
      cancelled = true
    }
    // Dependent credentials only need regeneration when their signing key changes.
  }, [detail.variables, jwtSigningKey])
  const valueFor = (
    variable: ServiceTemplateDetailResponse['variables'][number]
  ) => values[variable.name] || ''

  const missing = detail.variables.filter(
    (variable) =>
      variable.required &&
      !['public_url', 'public_host'].includes(variable.kind) &&
      !valueFor(variable).trim()
  )
  const missingCapabilityApprovals = detail.capability_requirements.filter(
    (requirement) => !approvedCapabilities[requirement.service]
  )

  const regenerateVariable = async (
    variable: ServiceTemplateDetailResponse['variables'][number]
  ) => {
    setGenerationError(null)
    try {
      const dependent = await generateDependentServiceTemplateValue(
        variable.kind,
        values
      )
      const next =
        dependent ??
        generateServiceTemplateValue(generatorInput(variable), detail.slug)
      setValues((current) => ({ ...current, [variable.name]: next }))
    } catch (error) {
      setGenerationError(
        getErrorMessage(error, `Could not regenerate ${variable.name}`)
      )
    }
  }

  const install = async () => {
    if (
      !detail.installable ||
      installing ||
      generatingDependencies ||
      generationError
    )
      return
    if (!name.trim()) {
      toast.error('Project name is required')
      return
    }
    if (missing.length > 0) {
      toast.error(
        `Fill required variables: ${missing.map((variable) => variable.name).join(', ')}`
      )
      return
    }
    if (missingCapabilityApprovals.length > 0) {
      toast.error(
        `Approve startup permissions for: ${missingCapabilityApprovals.map((requirement) => requirement.service).join(', ')}`
      )
      return
    }

    let createdProjectSlug: string | null = null
    setInstalling(true)
    setPreflightErrors([])
    try {
      const localVariables = Object.fromEntries(
        detail.variables
          .filter(
            (variable) =>
              variable.kind !== 'public_url' && variable.kind !== 'public_host'
          )
          .map((variable) => [variable.name, valueFor(variable)] as const)
          .filter(([, value]) => value.trim())
      )
      const dependentVariables = await Promise.all(
        detail.variables.map(
          async (variable) =>
            [
              variable.name,
              await generateDependentServiceTemplateValue(
                variable.kind,
                localVariables
              ),
            ] as const
        )
      )
      const resolvedVariables = {
        ...localVariables,
        ...Object.fromEntries(
          dependentVariables.filter(
            (entry): entry is readonly [string, string] => Boolean(entry[1])
          )
        ),
      }
      const approvedCapabilityServices = detail.capability_requirements
        .filter((requirement) => approvedCapabilities[requirement.service])
        .map((requirement) => requirement.service)
      const runPreflight = async () => {
        const response = await preflightServiceTemplate({
          throwOnError: true,
          path: { slug: detail.slug },
          body: {
            project_name: name.trim(),
            expected_install_plan_digest: detail.install_plan_digest,
            variables: resolvedVariables,
            approved_capability_services: approvedCapabilityServices,
          },
        })
        if (!response.data?.ready) {
          const errors = response.data?.errors || [
            'Temps could not validate this Compose template.',
          ]
          setPreflightErrors(errors)
          throw new Error(errors.join(' '))
        }
        return response.data
      }
      const preflight = await runPreflight()
      const archive = await prepareDrop([
        {
          file: new File([detail.compose], 'docker-compose.yml', {
            type: 'application/yaml',
          }),
          path: 'docker-compose.yml',
        },
      ])
      const publicPorts = detail.routes.map((route) => ({
        service: route.service,
        port: route.port,
      }))
      const createFromPreflight = (planned: typeof preflight) => {
        const finalVariables = {
          ...resolvedVariables,
          ...planned.public_variables,
        }
        return createProject({
          throwOnError: true,
          body: {
            name: name.trim(),
            expected_slug: planned.planned_project_slug,
            directory: '.',
            main_branch: 'main',
            preset: 'docker-compose',
            source_type: 'uploaded_source',
            project_type: 'server',
            automatic_deploy: false,
            storage_service_ids: [],
            preset_config: {
              composePath: 'docker-compose.yml',
              publicPorts,
              relaxedCapabilityServices: approvedCapabilityServices,
              templateOrigin: {
                provider: 'coolify',
                slug: detail.slug,
                sourceUrl: detail.source_url,
                sourceRevision: detail.source_revision,
                templateLastUpdatedAt: detail.template_last_updated_at,
              },
            },
            environment_variables: detail.variables
              .filter((variable) => finalVariables[variable.name]?.trim())
              .map((variable) => ({
                key: variable.name,
                value: finalVariables[variable.name],
                is_secret: variable.is_secret,
              })),
          },
        })
      }
      const createResult = await createServiceTemplateWithSlugRetry(
        preflight,
        createFromPreflight,
        runPreflight,
        (error) =>
          (
            extractProblemDetails(error) as
              | (ReturnType<typeof extractProblemDetails> & {
                  status?: number
                })
              | null
          )?.status === 409
      )
      const created = createResult.result
      if (!created.data) throw new Error('Temps created no project record')
      createdProjectSlug = created.data.slug

      const environments = await getEnvironments({
        throwOnError: true,
        path: { project_id: created.data.id },
      })
      const environment =
        environments.data?.find(
          (candidate) => candidate.name.toLowerCase() === 'production'
        ) || environments.data?.find((candidate) => !candidate.is_preview)
      if (!environment)
        throw new Error('The project has no production environment')

      const deployed = await deployFromUploadedSource({
        throwOnError: true,
        path: {
          project_id: created.data.id,
          environment_id: environment.id,
        },
        body: { file: archive.file },
      })
      if (!deployed.data) throw new Error('Temps accepted no deployment')
      toast.success(`${detail.name} installation started`)
      navigate(`/projects/${created.data.slug}/deployments/${deployed.data.id}`)
    } catch (error) {
      let message = getErrorMessage(error, 'Service installation failed')
      if (createdProjectSlug) {
        message += ` Project '${createdProjectSlug}' was kept so you can inspect or retry it safely.`
      }
      toast.error(message)
    } finally {
      setInstalling(false)
    }
  }

  return (
    <div className="space-y-6">
      <Button type="button" variant="ghost" size="sm" onClick={onBack}>
        <ArrowLeft className="mr-2 size-4" />
        Back to services
      </Button>

      <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <Card>
          <CardHeader>
            <div className="flex items-start gap-4">
              {templateLogo(detail)}
              <div className="min-w-0">
                <CardTitle>{detail.name}</CardTitle>
                <CardDescription className="mt-1">
                  {detail.description ||
                    'A self-hosted Docker Compose service.'}
                </CardDescription>
                <div className="mt-3 flex flex-wrap gap-2">
                  <Badge variant="outline">{detail.category}</Badge>
                  <Badge variant="outline">
                    {detail.service_count} containers
                  </Badge>
                  {detail.port && (
                    <Badge variant="outline">Port {detail.port}</Badge>
                  )}
                </div>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-6">
            {!detail.installable && (
              <Alert variant="destructive">
                <ShieldAlert className="size-4" />
                <AlertTitle>
                  This template needs manual compatibility work
                </AlertTitle>
                <AlertDescription>
                  <ul className="mt-2 list-disc space-y-1 pl-5">
                    {detail.compatibility_issues.map((issue) => (
                      <li key={issue}>{issue}</li>
                    ))}
                  </ul>
                  <p className="mt-3">
                    It remains visible so you can inspect the limitation; Temps
                    will not silently grant host access or start a partial
                    stack.
                  </p>
                </AlertDescription>
              </Alert>
            )}

            {preflightErrors.length > 0 && (
              <Alert variant="destructive">
                <AlertTriangle className="size-4" />
                <AlertTitle>Preflight did not pass</AlertTitle>
                <AlertDescription>
                  <ul className="mt-2 list-disc space-y-1 pl-5">
                    {preflightErrors.map((error) => (
                      <li key={error}>{error}</li>
                    ))}
                  </ul>
                </AlertDescription>
              </Alert>
            )}

            {detail.warnings.length > 0 && (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertTitle>Review before installing</AlertTitle>
                <AlertDescription>
                  <ul className="mt-2 list-disc space-y-1 pl-5">
                    {detail.warnings.map((warning) => (
                      <li key={warning}>{warning}</li>
                    ))}
                  </ul>
                </AlertDescription>
              </Alert>
            )}

            <div className="space-y-2">
              <Label htmlFor="service-project-name">Project name</Label>
              <Input
                id="service-project-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                disabled={!detail.installable || installing}
              />
              <p className="text-xs text-muted-foreground">
                Temps plans the final slug and canonical public URLs during
                preflight, then safely retries if another project claims it.
              </p>
            </div>

            {detail.routes.length > 0 && (
              <div className="space-y-3">
                <div>
                  <h3 className="font-medium">Public routes</h3>
                  <p className="text-sm text-muted-foreground">
                    Temps will expose only these Compose services. Other ports
                    stay private.
                  </p>
                </div>
                <div className="grid gap-2 sm:grid-cols-2">
                  {detail.routes.map((route) => (
                    <div
                      key={route.service}
                      className="rounded-lg border bg-muted/30 p-3"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <code className="text-sm">{route.service}</code>
                        <Badge variant="outline">:{route.port}</Badge>
                      </div>
                      <p className="mt-1 break-all text-xs text-muted-foreground">
                        Canonical hostname generated by Temps during validation
                      </p>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {detail.capability_requirements.length > 0 && (
              <div className="space-y-3">
                <div>
                  <h3 className="font-medium">Startup permissions</h3>
                  <p className="text-sm text-muted-foreground">
                    These images commonly need a limited capability set while
                    initializing their persistent directories. Approval is
                    explicit and scoped per service.
                  </p>
                </div>
                {detail.capability_requirements.map((requirement) => (
                  <label
                    key={requirement.service}
                    className="flex cursor-pointer items-start gap-3 rounded-lg border p-3"
                  >
                    <Checkbox
                      checked={Boolean(
                        approvedCapabilities[requirement.service]
                      )}
                      onCheckedChange={(checked) =>
                        setApprovedCapabilities((current) => ({
                          ...current,
                          [requirement.service]: checked === true,
                        }))
                      }
                      disabled={!detail.installable || installing}
                    />
                    <span className="space-y-1">
                      <span className="block text-sm font-medium">
                        Allow limited startup capabilities for{' '}
                        <code>{requirement.service}</code>
                      </span>
                      <span className="block text-xs text-muted-foreground">
                        {requirement.reason}
                      </span>
                    </span>
                  </label>
                ))}
              </div>
            )}

            {detail.variables.length > 0 && (
              <div className="space-y-4">
                <div>
                  <h3 className="font-medium">Configuration</h3>
                  <p className="text-sm text-muted-foreground">
                    Common Coolify magic values are generated locally. Review
                    any service-specific inputs before installation.
                  </p>
                </div>
                <div className="grid gap-4 md:grid-cols-2">
                  {detail.variables.map((variable) => {
                    const isSecret = variable.is_secret
                    const generated = serviceTemplateVariableIsGenerated(
                      variable.kind
                    )
                    return (
                      <div key={variable.name} className="space-y-2">
                        <div className="flex items-center justify-between gap-2">
                          <Label htmlFor={`service-variable-${variable.name}`}>
                            {variable.name}
                            {variable.required && (
                              <span className="text-destructive"> *</span>
                            )}
                          </Label>
                          {generated && (
                            <Badge variant="secondary" className="text-[10px]">
                              <Sparkles className="mr-1 size-3" />
                              Generated
                            </Badge>
                          )}
                        </div>
                        <div className="flex gap-2">
                          <Input
                            id={`service-variable-${variable.name}`}
                            type={
                              isSecret && !visible[variable.name]
                                ? 'password'
                                : 'text'
                            }
                            value={valueFor(variable)}
                            placeholder={
                              ['public_url', 'public_host'].includes(
                                variable.kind
                              )
                                ? 'Generated by Temps during preflight'
                                : undefined
                            }
                            onChange={(event) =>
                              setValues((current) => ({
                                ...current,
                                [variable.name]: event.target.value,
                              }))
                            }
                            disabled={
                              !detail.installable ||
                              installing ||
                              variable.kind === 'public_url' ||
                              variable.kind === 'public_host'
                            }
                            className="font-mono text-xs"
                          />
                          {isSecret && (
                            <Button
                              type="button"
                              variant="outline"
                              size="icon"
                              aria-label={
                                visible[variable.name]
                                  ? 'Hide value'
                                  : 'Show value'
                              }
                              onClick={() =>
                                setVisible((current) => ({
                                  ...current,
                                  [variable.name]: !current[variable.name],
                                }))
                              }
                            >
                              {visible[variable.name] ? (
                                <EyeOff className="size-4" />
                              ) : (
                                <Eye className="size-4" />
                              )}
                            </Button>
                          )}
                          {generated &&
                            !['public_url', 'public_host'].includes(
                              variable.kind
                            ) && (
                              <Button
                                type="button"
                                variant="outline"
                                size="icon"
                                aria-label={`Regenerate ${variable.name}`}
                                onClick={() =>
                                  void regenerateVariable(variable)
                                }
                              >
                                <RefreshCw className="size-4" />
                              </Button>
                            )}
                        </div>
                      </div>
                    )
                  })}
                </div>
              </div>
            )}
          </CardContent>
        </Card>

        <div className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">What Temps will do</CardTitle>
            </CardHeader>
            <CardContent>
              <ol className="space-y-3 text-sm text-muted-foreground">
                <li>
                  1. Validate variables, architecture, and Compose syntax.
                </li>
                <li>2. Create an uploaded-source Docker Compose project.</li>
                <li>3. Save configuration as project environment variables.</li>
                <li>
                  4. Copy this normalized Compose snapshot into the project.
                </li>
                <li>5. Run the normal Docker Compose deployment pipeline.</li>
              </ol>
              {detail.transformations.length > 0 && (
                <div className="mt-4 border-t pt-4">
                  <p className="text-sm font-medium">Safety transformations</p>
                  <ul className="mt-2 list-disc space-y-1 pl-5 text-xs text-muted-foreground">
                    {detail.transformations.map((transformation) => (
                      <li
                        key={`${transformation.code}-${transformation.description}`}
                      >
                        {transformation.description}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </CardContent>
          </Card>
          <Button
            type="button"
            className="w-full"
            size="lg"
            disabled={
              !detail.installable ||
              installing ||
              generatingDependencies ||
              Boolean(generationError) ||
              missing.length > 0 ||
              missingCapabilityApprovals.length > 0
            }
            onClick={() => void install()}
          >
            {installing ? (
              <Loader2 className="mr-2 size-4 animate-spin" />
            ) : (
              <Box className="mr-2 size-4" />
            )}
            {installing ? 'Installing…' : `Install ${detail.name}`}
          </Button>
          {generationError && (
            <Alert variant="destructive">
              <AlertTriangle className="size-4" />
              <AlertTitle>Credential generation failed</AlertTitle>
              <AlertDescription>{generationError}</AlertDescription>
            </Alert>
          )}
          {detail.documentation_url && (
            <Button variant="outline" className="w-full" asChild>
              <a
                href={detail.documentation_url}
                target="_blank"
                rel="noreferrer"
              >
                Service documentation
                <ExternalLink className="ml-2 size-4" />
              </a>
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}

export function ServiceTemplateCatalog() {
  const [search, setSearch] = useState('')
  const deferredSearch = useDeferredValue(search)
  const [category, setCategory] = useState('all')
  const [page, setPage] = useState(1)
  const [selectedSlug, setSelectedSlug] = useState<string | null>(null)

  const catalog = useQuery({
    ...listServiceTemplatesOptions({
      query: {
        search: deferredSearch || undefined,
        category: category === 'all' ? undefined : category,
        page,
        per_page: PER_PAGE,
      },
    }),
  })
  const detail = useQuery({
    ...getServiceTemplateOptions({
      path: { slug: selectedSlug || '' },
    }),
    enabled: Boolean(selectedSlug),
  })

  if (selectedSlug) {
    if (detail.isPending) {
      return (
        <div className="space-y-6" aria-label="Loading service template">
          <Skeleton className="h-9 w-36" />
          <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_22rem]">
            <Skeleton className="h-[38rem] w-full" />
            <Skeleton className="h-72 w-full" />
          </div>
        </div>
      )
    }
    if (detail.isError || !detail.data) {
      return (
        <div className="space-y-4">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setSelectedSlug(null)}
          >
            <ArrowLeft className="mr-2 size-4" />
            Back to services
          </Button>
          <CatalogError
            error={detail.error}
            retry={() => void detail.refetch()}
          />
        </div>
      )
    }
    return (
      <TemplateInstaller
        detail={detail.data}
        onBack={() => setSelectedSlug(null)}
      />
    )
  }

  if (catalog.isPending) {
    return (
      <div className="space-y-6" aria-label="Loading service catalog">
        <div className="space-y-3">
          <Skeleton className="h-7 w-52" />
          <Skeleton className="h-4 w-full max-w-xl" />
          <Skeleton className="h-6 w-80" />
        </div>
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 6 }, (_, index) => (
            <Skeleton key={index} className="h-64 w-full" />
          ))}
        </div>
      </div>
    )
  }
  if (catalog.isError || !catalog.data) {
    return (
      <CatalogError
        error={catalog.error}
        retry={() => void catalog.refetch()}
      />
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
        <div>
          <h2 className="text-xl font-semibold">Install a service</h2>
          <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
            Deploy from {catalog.data.catalog_total} community-maintained Docker
            Compose templates. Temps reviews compatibility before anything
            reaches Docker.
          </p>
          <div className="mt-3 flex flex-wrap gap-2">
            <Badge
              variant="secondary"
              className="border-emerald-500/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300"
            >
              {catalog.data.compatibility.standard} ready
            </Badge>
            <Badge className="border-amber-500/20 bg-amber-500/10 text-amber-700 dark:text-amber-300">
              {catalog.data.compatibility.elevated} need approval
            </Badge>
            <Badge variant="outline" className="text-muted-foreground">
              {catalog.data.compatibility.blocked} need manual work
            </Badge>
          </div>
        </div>
        <div className="flex flex-col gap-2 sm:flex-row">
          <div className="relative sm:w-72">
            <Search className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(event) => {
                setSearch(event.target.value)
                setPage(1)
              }}
              placeholder="Search services…"
              className="pl-9"
            />
          </div>
          <Select
            value={category}
            onValueChange={(value) => {
              setCategory(value)
              setPage(1)
            }}
          >
            <SelectTrigger className="sm:w-48">
              <SelectValue placeholder="Category" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All categories</SelectItem>
              {catalog.data.categories.map((item) => (
                <SelectItem key={item} value={item}>
                  {item}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      {catalog.data.templates.length === 0 ? (
        <Card>
          <CardContent className="py-12 text-center">
            <p className="font-medium">No services match this search</p>
            <p className="mt-1 text-sm text-muted-foreground">
              Try another name or clear the category filter.
            </p>
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {catalog.data.templates.map((template) => (
            <TemplateCard
              key={template.slug}
              template={template}
              onSelect={() => setSelectedSlug(template.slug)}
            />
          ))}
        </div>
      )}

      <div className="flex flex-col gap-3 border-t pt-4 text-sm text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
        <p>
          Catalog by{' '}
          <a
            href={catalog.data.source_repository_url}
            target="_blank"
            rel="noreferrer"
            className="underline underline-offset-4 hover:text-foreground"
          >
            Coolify
          </a>{' '}
          (Apache-2.0). Each install keeps its own Compose snapshot.
        </p>
        <div className="flex items-center gap-2">
          <span>
            Page {catalog.data.page} of {Math.max(1, catalog.data.total_pages)}
          </span>
          <Button
            variant="outline"
            size="icon"
            disabled={page <= 1}
            onClick={() => setPage((current) => Math.max(1, current - 1))}
            aria-label="Previous services page"
          >
            <ChevronLeft className="size-4" />
          </Button>
          <Button
            variant="outline"
            size="icon"
            disabled={page >= catalog.data.total_pages}
            onClick={() => setPage((current) => current + 1)}
            aria-label="Next services page"
          >
            <ChevronRight className="size-4" />
          </Button>
        </div>
      </div>
    </div>
  )
}

// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, useEffect, useMemo, useCallback, useRef } from 'react'
import { useNavigate } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod/v4'
import { toast } from 'sonner'
import { format } from 'date-fns'
import {
  createProjectFromTemplateMutation,
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  CreatableServiceTypeRoute,
  TemplateResponse,
  ConnectionResponse,
  ExternalServiceInfo,
} from '@/api/client/types.gen'
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
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
  FormDescription,
} from '@/components/ui/form'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Label } from '@/components/ui/label'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ServiceLogo } from '@/components/ui/service-logo'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import {
  ProvidedEnvironmentVariables,
  ProvidedEnvironmentVariableWarning,
} from '@/components/project/ProvidedEnvironmentVariables'
import type { ProvidedEnvironmentVariableCollision } from '@/lib/provided-environment-variables'
import { TemplateImage } from '@/components/templates/TemplateImage'
import {
  runGenerator,
  generatorDependsOnRepoName,
  resolveDeploymentUrlBase,
} from '@/components/templates/envVarGenerators'
import { useSettings } from '@/hooks/useSettings'
import { getErrorMessage } from '@/utils/errorHandling'
import { cn } from '@/lib/utils'
import { ADD_SERVICE_TYPES } from '@/lib/addServiceTypes'
import {
  isLikelySecretProjectEnvironmentVariable,
  isTempsManagedProjectEnvironmentVariable,
  projectEnvironmentVariablesSchema,
} from '@/lib/project-environment-variables'
import {
  getTemplateServiceRequirements,
  normalizeTemplateServiceType,
  toggleDatabaseSelection,
} from '@/lib/template-service-requirements'
import { useAllServices } from '@/hooks/useAllServices'
import {
  AlertCircle,
  Building2,
  CheckCircle2,
  ChevronDown,
  Database,
  Eye,
  EyeOff,
  ExternalLink,
  GitBranch,
  Loader2,
  Lock,
  Plus,
  Rocket,
  Settings,
  Sparkles,
  Star,
  User,
  X,
} from 'lucide-react'
import Github from '@/icons/Github'
import Gitlab from '@/icons/Gitlab'

const EMPTY_SERVICE_IDS: number[] = []

/**
 * Renders the correct icon for a Git provider type — used in the connection
 * picker so a GitLab connection doesn't show the GitHub mark.
 */
function ProviderIcon({
  providerType,
  className = 'h-4 w-4',
}: {
  providerType: string | undefined
  className?: string
}) {
  if (providerType === 'github' || providerType === 'github_app') {
    return <Github className={className} />
  }
  if (providerType === 'gitlab') {
    return <Gitlab className={className} />
  }
  return <GitBranch className={className} />
}

// Form schema
const formSchema = z.object({
  projectName: z.string().min(1, 'Project name is required'),
  repositoryName: z.string().min(1, 'Repository name is required'),
  repositoryOwner: z.string().optional(),
  // Optional: when omitted the project deploys directly from the template's
  // public source repository (one-click, no Git account required) instead of
  // forking it into the user's Git provider.
  gitProviderConnectionId: z.number().optional(),
  private: z.boolean(),
  automaticDeploy: z.boolean(),
  storageServices: z.array(z.number()),
  environmentVariables: projectEnvironmentVariablesSchema,
})

type FormValues = z.infer<typeof formSchema>

// Repository URL Preview Component
interface RepositoryPreviewProps {
  repositoryName: string
  repositoryOwner?: string
  connection?: ConnectionResponse
}

function RepositoryPreview({
  repositoryName,
  repositoryOwner,
  connection,
}: RepositoryPreviewProps) {
  if (!repositoryName || !connection) return null

  const owner = repositoryOwner || connection.account_name
  const repoUrl = `github.com/${owner}/${repositoryName}`

  return (
    <div className="rounded-lg border bg-muted/50 p-4">
      <div className="flex items-center gap-2 text-sm">
        <GitBranch className="h-4 w-4 text-muted-foreground" />
        <span className="text-muted-foreground">
          Repository will be created at:
        </span>
      </div>
      <div className="mt-2 flex items-center gap-2">
        <code className="flex-1 rounded bg-background px-3 py-2 font-mono text-sm">
          {repoUrl}
        </code>
        <a
          href={`https://${repoUrl}`}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center justify-center rounded-md text-sm font-medium ring-offset-background transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:pointer-events-none disabled:opacity-50 border border-input bg-background hover:bg-accent hover:text-accent-foreground h-9 w-9"
          title="Preview (will not exist until created)"
        >
          <ExternalLink className="h-4 w-4" />
        </a>
      </div>
    </div>
  )
}

interface TemplateConfiguratorProps {
  template: TemplateResponse
  onCancel?: () => void
  onSuccess?: () => void
  className?: string
}

export function TemplateConfigurator({
  template,
  onCancel,
  onSuccess,
  className,
}: TemplateConfiguratorProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // State
  const [showSecrets, setShowSecrets] = useState<Record<number, boolean>>({})
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<CreatableServiceTypeRoute | null>(null)
  const [newlyCreatedServices, setNewlyCreatedServices] = useState<
    ExternalServiceInfo[]
  >([])
  const [providedEnvironmentVariables, setProvidedEnvironmentVariables] =
    useState<ProvidedEnvironmentVariableCollision[]>([])

  // Fetch connections
  const { data: connectionsData, isLoading: isLoadingConnections } = useQuery({
    ...listConnectionsOptions(),
  })

  // Fetch git providers so we can render the right icon per connection
  // (a GitLab connection should not show the GitHub mark).
  const { data: gitProviders } = useQuery({
    ...listGitProvidersOptions(),
  })

  const providerTypeForConnection = (
    conn: ConnectionResponse
  ): string | undefined =>
    gitProviders?.find((p) => p.id === conn.provider_id)?.provider_type

  // Fetch existing services
  const {
    data: existingServices,
    isPending: isLoadingServices,
    isError: isServicesError,
    refetch: refetchServices,
  } = useAllServices()
  const availableServices = useMemo(() => {
    const servicesById = new Map<number, ExternalServiceInfo>()
    existingServices?.forEach((service) =>
      servicesById.set(service.id, service)
    )
    newlyCreatedServices.forEach((service) =>
      servicesById.set(service.id, service)
    )
    return Array.from(servicesById.values())
  }, [existingServices, newlyCreatedServices])

  // Platform settings provide `preview_domain` (used for deployment URLs) and
  // `external_url`. These drive the `app_url` env-var generator so generated
  // URLs match the proxy's actual routing rules instead of guessing `temps.sh`.
  const { data: platformSettings } = useSettings()

  // Generate default repo name from project name
  const generateRepoName = (projectName: string) => {
    return projectName
      .toLowerCase()
      .replace(/[^a-z0-9-]/g, '-')
      .replace(/-+/g, '-')
      .replace(/^-|-$/g, '')
  }

  const initialRepoName = generateRepoName(template.name)
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

  const configurableTemplateEnvVars = useMemo(
    () =>
      template.env_vars.filter(
        (env) => !isTempsManagedProjectEnvironmentVariable(env.name)
      ),
    [template.env_vars]
  )

  // Initialize form with template defaults, running any default_generator on
  // mount so required fields like NEXTAUTH_URL / NEXTAUTH_SECRET start filled.
  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit',
    defaultValues: {
      projectName: template.name,
      repositoryName: initialRepoName,
      repositoryOwner: undefined,
      gitProviderConnectionId: undefined as unknown as number,
      private: true,
      automaticDeploy: true,
      storageServices: [],
      environmentVariables: configurableTemplateEnvVars.map((env) => {
        const generated =
          runGenerator(env.default_generator, {
            repositoryName: initialRepoName,
            base: deploymentUrlBase,
          }) || ''
        return {
          key: env.name,
          value: env.default || generated,
          isSecret:
            isLikelySecretProjectEnvironmentVariable(env.name) ||
            env.default_generator?.includes('secret') === true,
        }
      }),
    },
  })

  // Track which generator-produced values are still "untouched" by the user so
  // we can re-run repo-name-dependent generators (`app_url`) when the slug changes.
  // Keyed by env-var name, value is the last value we generated.
  const [autoGenerated, setAutoGenerated] = useState<Record<string, string>>(
    () => {
      const seeded: Record<string, string> = {}
      for (const env of configurableTemplateEnvVars) {
        const value =
          runGenerator(env.default_generator, {
            repositoryName: initialRepoName,
            base: deploymentUrlBase,
          }) || ''
        if (value && !env.default) seeded[env.name] = value
      }
      return seeded
    }
  )

  // Auto-select first connection when available. Skipped for image-based
  // templates, which deploy a prebuilt image and never touch Git.
  useEffect(() => {
    if (template.image) return
    if (
      connectionsData?.connections?.length &&
      !form.getValues('gitProviderConnectionId')
    ) {
      form.setValue(
        'gitProviderConnectionId',
        connectionsData.connections[0].id,
        {
          shouldValidate: true,
        }
      )
    }
  }, [connectionsData, form, template.image])

  // Auto-select an existing service that satisfies a template requirement, so a
  // returning user who already has (say) a Postgres service gets it attached
  // with zero extra clicks — true "almost one-click" deploy. We only pre-select
  // when nothing is selected yet, and never override the user's choices. New
  // users with no matching service create one via the "Add Service" button.
  const autoSelectedServicesRef = useRef(false)
  useEffect(() => {
    if (autoSelectedServicesRef.current) return
    if (availableServices.length === 0) return
    if (template.services.length === 0) return
    if ((form.getValues('storageServices') || []).length > 0) return

    const wanted = new Set(
      template.services.map((serviceType) =>
        normalizeTemplateServiceType(serviceType)
      )
    )
    const matchIds: number[] = []
    for (const required of wanted) {
      // First existing service whose type matches the required engine.
      const match = availableServices.find(
        (svc: ExternalServiceInfo) =>
          normalizeTemplateServiceType(svc.service_type) === required &&
          !matchIds.includes(svc.id)
      )
      if (match) matchIds.push(match.id)
    }

    if (matchIds.length > 0) {
      autoSelectedServicesRef.current = true
      form.setValue('storageServices', matchIds, { shouldValidate: false })
    }
  }, [availableServices, template.services, form])

  // Watch project name to update repo name
  const projectName = useWatch({ control: form.control, name: 'projectName' })
  useEffect(() => {
    if (projectName) {
      form.setValue('repositoryName', generateRepoName(projectName), {
        shouldValidate: false,
      })
    }
  }, [projectName, form])

  // When the repo name OR resolved deployment URL base changes, re-run
  // generators that depend on those inputs (e.g. `app_url` -> NEXTAUTH_URL).
  // We only overwrite the form value if the user hasn't manually edited it —
  // detected by comparing against the last value we wrote ourselves.
  //
  // `deploymentUrlBase` is included so values get re-computed once
  // `useSettings` returns the platform's actual `preview_domain` /
  // `external_url` (the first render uses the browser-host fallback, which
  // is wrong on local dev). Tracking it as a serialized string avoids
  // re-running on every memo identity change.
  const baseKey = `${deploymentUrlBase.scheme}://${deploymentUrlBase.host}${
    deploymentUrlBase.port ? `:${deploymentUrlBase.port}` : ''
  }`
  const repositoryNameWatch = useWatch({
    control: form.control,
    name: 'repositoryName',
  })
  useEffect(() => {
    if (!repositoryNameWatch) return
    const currentVars = form.getValues('environmentVariables') || []
    const nextAutoGenerated = { ...autoGenerated }
    let mutated = false

    configurableTemplateEnvVars.forEach((envTemplate) => {
      if (!generatorDependsOnRepoName(envTemplate.default_generator)) return

      const idx = currentVars.findIndex((v) => v.key === envTemplate.name)
      if (idx === -1) return

      const currentValue = currentVars[idx].value
      const lastAuto = autoGenerated[envTemplate.name]
      const isUntouched = currentValue === '' || currentValue === lastAuto
      if (!isUntouched) return

      const newValue =
        runGenerator(envTemplate.default_generator, {
          repositoryName: repositoryNameWatch,
          base: deploymentUrlBase,
        }) || ''
      if (newValue && newValue !== currentValue) {
        form.setValue(`environmentVariables.${idx}.value`, newValue, {
          shouldValidate: false,
        })
        nextAutoGenerated[envTemplate.name] = newValue
        mutated = true
      }
    })

    if (mutated) setAutoGenerated(nextAutoGenerated)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repositoryNameWatch, baseKey])

  // Create project mutation
  const createFromTemplateMutation = useMutation({
    ...createProjectFromTemplateMutation(),
    onSuccess: async (data) => {
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success(`Project "${data.project_name}" created successfully!`)
      onSuccess?.()
      navigate(`/projects/${data.project_slug}?new=true`)
    },
    onError: (error) => {
      // The backend returns RFC 7807 Problem Details, which surface their
      // message via `detail` / `title` rather than `error.message` (which is
      // `undefined` and previously rendered as "Failed to create project: undefined").
      const message = getErrorMessage(error, 'Unknown error')
      toast.error(`Failed to create project: ${message}`)
      console.error('Template project creation failed:', error)
    },
  })

  // Service toggle handler
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const result = toggleDatabaseSelection(
        currentServices,
        serviceId,
        availableServices
      )
      if (result.conflictingService) {
        toast.error('A compatible database is already selected', {
          description: `${result.conflictingService.name} already provides this database variable namespace. Deselect it first.`,
        })
        return
      }
      form.setValue('storageServices', result.selectedServiceIds)
    },
    [form, availableServices]
  )

  // Form submission
  const handleSubmit = async (data: FormValues) => {
    if (isLoadingServices) {
      toast.error('Wait for the database list to finish loading.')
      return
    }
    if (isServicesError) {
      toast.error('Reload the database list before creating this project.')
      return
    }
    const missingServiceRequirements = getTemplateServiceRequirements(
      template.services,
      availableServices,
      data.storageServices || []
    ).filter((requirement) => !requirement.isSatisfied)
    if (missingServiceRequirements.length > 0) {
      toast.error(
        `Select ${missingServiceRequirements.map((requirement) => requirement.label).join(', ')} before creating this project.`
      )
      return
    }

    // Creation selects the new service immediately, but the submitted value
    // remains the form's visible selection so a later deselect is respected.
    const allServiceIds = Array.from(new Set(data.storageServices || []))

    // No connection selected → one-click public-repo deploy. The backend forks
    // the template when a connection is present, and deploys straight from the
    // template's public source repo when it isn't. Repository name/owner only
    // matter in fork mode, so they're omitted otherwise.
    const usePublicRepo = data.gitProviderConnectionId == null

    await createFromTemplateMutation.mutateAsync({
      body: {
        template_slug: template.slug,
        project_name: data.projectName,
        git_provider_connection_id: data.gitProviderConnectionId ?? undefined,
        repository_name: usePublicRepo ? undefined : data.repositoryName,
        repository_owner: usePublicRepo
          ? undefined
          : data.repositoryOwner || undefined,
        private: data.private,
        // Auto-deploy on push is only possible against a fork we own.
        automatic_deploy: usePublicRepo ? false : data.automaticDeploy,
        storage_service_ids: allServiceIds,
        environment_variables: data.environmentVariables
          .filter((env) => env.key && env.value)
          .map((env) => ({
            name: env.key,
            value: env.value,
            is_secret: env.isSecret,
          })),
      },
    })
  }

  // Add environment variable
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      [...currentVars, { key: '', value: '', isSecret: false }],
      { shouldValidate: false }
    )
  }

  // Remove environment variable
  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  const watchedServices =
    useWatch({ control: form.control, name: 'storageServices' }) ??
    EMPTY_SERVICE_IDS
  const watchedEnvVars = useWatch({
    control: form.control,
    name: 'environmentVariables',
  })
  const watchedConnectionId = useWatch({
    control: form.control,
    name: 'gitProviderConnectionId',
  })
  const serviceRequirements = useMemo(
    () =>
      getTemplateServiceRequirements(
        template.services,
        availableServices,
        watchedServices
      ),
    [template.services, availableServices, watchedServices]
  )
  const missingServiceRequirements = serviceRequirements.filter(
    (requirement) => !requirement.isSatisfied
  )

  // Public-repo (one-click) mode when no Git connection is selected. The
  // fork-only fields (repository name/owner/visibility) are hidden in this mode.
  const usePublicRepo = watchedConnectionId == null

  // Image-based template: deploys a prebuilt image directly (no build, no Git).
  // The backend decides image-vs-build from `template.image`; when it's set we
  // hide the entire Git/source section and show an "instant deploy" note.
  const isImageTemplate = Boolean(template.image)

  // Check if required env vars are filled
  const missingRequiredVars = useMemo(() => {
    const requiredEnvVars = configurableTemplateEnvVars.filter(
      (environmentVariable) => environmentVariable.required
    )
    return requiredEnvVars.filter((required) => {
      const current = (watchedEnvVars ?? []).find(
        (environmentVariable) => environmentVariable.key === required.name
      )
      return !current?.value
    })
  }, [configurableTemplateEnvVars, watchedEnvVars])

  if (isLoadingConnections) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  // No early dead-end when there's no Git connection. A brand-new user with no
  // provider linked can still deploy the demo directly from the template's
  // public source repo (one-click activation). The connection picker below
  // becomes optional and the form deploys in public-repo mode.

  return (
    <div className={cn('space-y-6', className)}>
      {/* Template Info Header */}
      <Card>
        <CardHeader className="pb-3">
          <div className="flex items-start justify-between">
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
                <CardTitle className="text-lg flex items-center gap-2">
                  {template.name}
                  {template.is_featured && (
                    <Star className="h-4 w-4 text-yellow-500 fill-yellow-500" />
                  )}
                </CardTitle>
                <CardDescription>{template.description}</CardDescription>
              </div>
            </div>
            <Badge variant="secondary">{template.preset}</Badge>
          </div>
        </CardHeader>
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
          {template.services.length > 0 && (
            <div className="mt-2 flex items-center gap-2 text-sm text-muted-foreground">
              <Database className="h-4 w-4" />
              <span>Requires: {template.services.join(', ')}</span>
            </div>
          )}
        </CardContent>
      </Card>

      <Form {...form}>
        <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
          {/* Project Configuration */}
          <Card>
            <CardHeader>
              <CardTitle>Project Configuration</CardTitle>
              <CardDescription>Configure your new project</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <FormField
                control={form.control}
                name="projectName"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Project Name</FormLabel>
                    <FormControl>
                      <Input {...field} placeholder="My Awesome Project" />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />

              {/* Image-based template: deploys a prebuilt image directly. No
                  Git source/connection needed, so the whole source picker is
                  replaced by an "instant deploy" note. */}
              {isImageTemplate && (
                <div className="flex items-start gap-3 rounded-md border border-primary/30 bg-primary/5 px-3 py-3">
                  <Rocket className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
                  <div className="space-y-1">
                    <p className="text-sm font-medium">
                      Deploys instantly from a prebuilt image
                    </p>
                    <p className="text-xs text-muted-foreground">
                      No build step and no Git account — Temps pulls{' '}
                      <code className="rounded bg-muted px-1 py-0.5 text-[11px]">
                        {template.image}
                      </code>{' '}
                      and runs it in seconds.
                    </p>
                  </div>
                </div>
              )}

              {!isImageTemplate && (
                <FormField
                  control={form.control}
                  name="gitProviderConnectionId"
                  render={({ field }) => {
                    const conns = connectionsData?.connections ?? []
                    const setValue = (id: number) => field.onChange(id)

                    // No connection: deploy straight from the template's public
                    // source repo. This is the one-click activation path — no Git
                    // account required. We surface a "connect to fork instead"
                    // affordance for users who want their own copy.
                    if (conns.length === 0) {
                      return (
                        <FormItem>
                          <FormLabel>Source</FormLabel>
                          <div className="flex items-start gap-3 rounded-md border bg-muted/50 px-3 py-3">
                            <Rocket className="mt-0.5 h-4 w-4 shrink-0 text-primary" />
                            <div className="space-y-1">
                              <p className="text-sm font-medium">
                                Deploy from the template&apos;s public source
                              </p>
                              <p className="text-xs text-muted-foreground">
                                No Git account needed — Temps deploys directly
                                from the template repository. Want your own copy
                                to push to?{' '}
                                <button
                                  type="button"
                                  onClick={() => navigate('/git-providers')}
                                  className="underline underline-offset-2 hover:text-foreground"
                                >
                                  Connect a Git provider
                                </button>{' '}
                                to fork it instead.
                              </p>
                            </div>
                          </div>
                          <FormMessage />
                        </FormItem>
                      )
                    }

                    // Single connection: render as a read-only chip. The form
                    // value is auto-set in the existing useEffect that picks
                    // the first connection on mount, so no extra wiring needed.
                    if (conns.length === 1) {
                      const only = conns[0]
                      return (
                        <FormItem>
                          <FormLabel>Git Provider</FormLabel>
                          <div className="flex items-center gap-2 rounded-md border bg-muted/50 px-3 py-2">
                            <ProviderIcon
                              providerType={providerTypeForConnection(only)}
                            />
                            <span className="font-medium">
                              {only.account_name}
                            </span>
                            <span className="text-xs text-muted-foreground">
                              ({only.account_type})
                            </span>
                          </div>
                          <FormDescription>
                            A new repository will be created in your connected
                            Git account
                          </FormDescription>
                          <FormMessage />
                        </FormItem>
                      )
                    }

                    // 2-4 connections: radio cards (clickable rows). Easier to
                    // scan than a dropdown when the list is short.
                    if (conns.length >= 2 && conns.length <= 4) {
                      return (
                        <FormItem>
                          <FormLabel>Git Provider</FormLabel>
                          <FormControl>
                            <RadioGroup
                              value={field.value?.toString() ?? ''}
                              onValueChange={(v) => setValue(parseInt(v, 10))}
                              className="gap-2"
                            >
                              {conns.map((conn: ConnectionResponse) => {
                                const id = `git-conn-${conn.id}`
                                const checked = field.value === conn.id
                                return (
                                  <Label
                                    key={conn.id}
                                    htmlFor={id}
                                    className={cn(
                                      'flex cursor-pointer items-center gap-3 rounded-md border p-3 transition-colors hover:bg-accent/50',
                                      checked && 'border-primary bg-accent/50'
                                    )}
                                  >
                                    <RadioGroupItem
                                      id={id}
                                      value={conn.id.toString()}
                                    />
                                    <ProviderIcon
                                      providerType={providerTypeForConnection(
                                        conn
                                      )}
                                    />
                                    <span className="font-medium">
                                      {conn.account_name}
                                    </span>
                                    <span className="text-xs text-muted-foreground">
                                      ({conn.account_type})
                                    </span>
                                  </Label>
                                )
                              })}
                            </RadioGroup>
                          </FormControl>
                          <FormDescription>
                            A new repository will be created in your selected
                            Git account
                          </FormDescription>
                          <FormMessage />
                        </FormItem>
                      )
                    }

                    // 5+ connections: fall back to a select dropdown.
                    return (
                      <FormItem>
                        <FormLabel>Git Provider</FormLabel>
                        <Select
                          value={field.value?.toString()}
                          onValueChange={(v) => setValue(parseInt(v, 10))}
                        >
                          <FormControl>
                            <SelectTrigger>
                              <SelectValue placeholder="Select a Git provider connection" />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            {conns.map((conn: ConnectionResponse) => (
                              <SelectItem
                                key={conn.id}
                                value={conn.id.toString()}
                              >
                                <div className="flex items-center gap-2">
                                  <ProviderIcon
                                    providerType={providerTypeForConnection(
                                      conn
                                    )}
                                  />
                                  <span>{conn.account_name}</span>
                                  <span className="text-xs text-muted-foreground">
                                    ({conn.account_type})
                                  </span>
                                </div>
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <FormDescription>
                          A new repository will be created in your connected Git
                          account
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )
                  }}
                />
              )}

              {/* Repository name/owner/visibility only apply when forking into
                  a Git account. In public-repo (one-click) mode there's no fork,
                  so these are hidden to keep the path frictionless. */}
              {!isImageTemplate && !usePublicRepo && (
                <FormField
                  control={form.control}
                  name="repositoryName"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Repository Name</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="my-awesome-project" />
                      </FormControl>
                      <FormDescription>
                        This will be the name of the new repository created from
                        the template
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}

              {!usePublicRepo && (
                <FormField
                  control={form.control}
                  name="repositoryOwner"
                  render={({ field }) => {
                    const selectedConnection =
                      connectionsData?.connections?.find(
                        (c: ConnectionResponse) => c.id === watchedConnectionId
                      )
                    return (
                      <FormItem>
                        <FormLabel>Repository Owner</FormLabel>
                        <Select
                          value={field.value || '_personal'}
                          onValueChange={(v) =>
                            field.onChange(v === '_personal' ? undefined : v)
                          }
                        >
                          <FormControl>
                            <SelectTrigger>
                              <SelectValue placeholder="Select repository owner" />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            <SelectItem value="_personal">
                              <div className="flex items-center gap-2">
                                <User className="h-4 w-4" />
                                <span>Personal Account</span>
                                <span className="text-xs text-muted-foreground">
                                  (Your personal repositories)
                                </span>
                              </div>
                            </SelectItem>
                            {selectedConnection &&
                              selectedConnection.account_type ===
                                'Organization' && (
                                <SelectItem
                                  value={selectedConnection.account_name}
                                >
                                  <div className="flex items-center gap-2">
                                    <Building2 className="h-4 w-4" />
                                    <span>
                                      {selectedConnection.account_name}
                                    </span>
                                    <span className="text-xs text-muted-foreground">
                                      (Organization)
                                    </span>
                                  </div>
                                </SelectItem>
                              )}
                          </SelectContent>
                        </Select>
                        <FormDescription>
                          Choose where to create the repository
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )
                  }}
                />
              )}

              {/* Repository URL Preview (fork mode only) */}
              {!usePublicRepo && (
                <RepositoryPreview
                  repositoryName={form.watch('repositoryName')}
                  repositoryOwner={form.watch('repositoryOwner')}
                  connection={connectionsData?.connections?.find(
                    (c: ConnectionResponse) => c.id === watchedConnectionId
                  )}
                />
              )}

              {!usePublicRepo && (
                <div className="flex flex-col gap-4 sm:flex-row">
                  <FormField
                    control={form.control}
                    name="private"
                    render={({ field }) => (
                      <FormItem className="flex-1 flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
                        <FormControl>
                          <Checkbox
                            checked={field.value}
                            onCheckedChange={field.onChange}
                          />
                        </FormControl>
                        <div className="space-y-1 leading-none">
                          <FormLabel className="flex items-center gap-2">
                            <Lock className="h-4 w-4" />
                            Private Repository
                          </FormLabel>
                          <p className="text-sm text-muted-foreground">
                            Create a private repository
                          </p>
                        </div>
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="automaticDeploy"
                    render={({ field }) => (
                      <FormItem className="flex-1 flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
                        <FormControl>
                          <Checkbox
                            checked={field.value}
                            onCheckedChange={field.onChange}
                          />
                        </FormControl>
                        <div className="space-y-1 leading-none">
                          <FormLabel className="flex items-center gap-2">
                            <GitBranch className="h-4 w-4" />
                            Automatic Deployments
                          </FormLabel>
                          <p className="text-sm text-muted-foreground">
                            Deploy when code is pushed
                          </p>
                        </div>
                      </FormItem>
                    )}
                  />
                </div>
              )}
            </CardContent>
          </Card>

          {/* Databases */}
          <Card>
            <CardHeader>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="space-y-1.5">
                  <CardTitle>Databases</CardTitle>
                  <CardDescription>
                    Link a managed database or storage resource. Connection
                    variables are injected automatically.
                  </CardDescription>
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      disabled={isLoadingServices || isServicesError}
                    >
                      <Plus className="h-4 w-4 mr-2" />
                      Add Database
                      <ChevronDown className="h-4 w-4 ml-1" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="w-64">
                    {ADD_SERVICE_TYPES.map((type) => {
                      const isTypeAlreadySelected = availableServices.some(
                        (service) =>
                          watchedServices.includes(service.id) &&
                          normalizeTemplateServiceType(service.service_type) ===
                            type.id
                      )
                      return (
                        <DropdownMenuItem
                          key={type.id}
                          onClick={() => {
                            if (isTypeAlreadySelected) {
                              toast.error(
                                `A ${type.name} database is already selected`,
                                {
                                  description:
                                    'Deselect it before creating another database of this type.',
                                }
                              )
                              return
                            }
                            setSelectedServiceType(type.id)
                            setIsCreateServiceDialogOpen(true)
                          }}
                          className={cn(
                            'flex items-center gap-3 py-2.5',
                            isTypeAlreadySelected &&
                              'cursor-not-allowed opacity-50'
                          )}
                        >
                          <ServiceLogo service={type.id} className="h-6 w-6" />
                          <div className="flex flex-col">
                            <span className="font-medium">{type.name}</span>
                            <span className="text-xs text-muted-foreground">
                              {type.description}
                            </span>
                          </div>
                        </DropdownMenuItem>
                      )
                    })}
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            </CardHeader>
            <CardContent>
              {isLoadingServices && (
                <div className="flex items-center gap-2 rounded-md border p-4 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  Loading available databases…
                </div>
              )}

              {isServicesError && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription className="flex flex-wrap items-center justify-between gap-3">
                    <span>
                      Could not load your databases. Retry before creating or
                      selecting one to avoid duplicates.
                    </span>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => void refetchServices()}
                    >
                      Retry
                    </Button>
                  </AlertDescription>
                </Alert>
              )}

              {!isLoadingServices &&
                !isServicesError &&
                serviceRequirements.length > 0 && (
                  <div className="mb-5 space-y-3 rounded-lg border border-primary/25 bg-primary/[0.035] p-4">
                    <div className="flex items-start gap-3">
                      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-primary/25 bg-background">
                        <Database className="h-4 w-4 text-primary" />
                      </div>
                      <div>
                        <p className="text-sm font-medium">
                          Required for this template
                        </p>
                        <p className="text-xs text-muted-foreground">
                          Select a compatible database or create one here. Temps
                          will attach it and inject its connection variables.
                        </p>
                      </div>
                    </div>

                    {serviceRequirements.map((requirement) => (
                      <div
                        key={requirement.key}
                        className="rounded-md border bg-background/80 p-3"
                      >
                        <div className="flex flex-wrap items-center gap-2">
                          {requirement.serviceType ? (
                            <ServiceLogo
                              service={requirement.serviceType}
                              className="h-6 w-6"
                            />
                          ) : (
                            <Database className="h-6 w-6 text-muted-foreground" />
                          )}
                          <span className="text-sm font-medium">
                            {requirement.label}
                          </span>
                          <Badge
                            variant={
                              requirement.isSatisfied ? 'secondary' : 'outline'
                            }
                            className={cn(
                              'ml-auto',
                              requirement.isSatisfied &&
                                'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300'
                            )}
                          >
                            {requirement.isSatisfied ? 'Selected' : 'Required'}
                          </Badge>
                        </div>

                        {requirement.isSatisfied ? (
                          <div className="mt-3 flex items-center gap-2 text-xs text-emerald-700 dark:text-emerald-300">
                            <CheckCircle2 className="h-4 w-4" />
                            <span>
                              {requirement.selectedServices
                                .map((service) => service.name)
                                .join(', ')}{' '}
                              will be linked automatically.
                            </span>
                          </div>
                        ) : requirement.availableServices.length > 0 ? (
                          <div className="mt-3 space-y-2">
                            <p className="text-xs text-muted-foreground">
                              Select an existing {requirement.label} database:
                            </p>
                            <div className="flex flex-wrap gap-2">
                              {requirement.availableServices.map((service) => (
                                <Button
                                  key={service.id}
                                  type="button"
                                  variant="outline"
                                  size="sm"
                                  onClick={() =>
                                    handleServiceToggle(service.id)
                                  }
                                >
                                  <CheckCircle2 className="mr-2 h-4 w-4" />
                                  Select {service.name}
                                </Button>
                              ))}
                            </div>
                          </div>
                        ) : requirement.serviceType ? (
                          <div className="mt-3 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                            <p className="text-xs text-muted-foreground">
                              You do not have a {requirement.label} database
                              yet.
                            </p>
                            <Button
                              type="button"
                              size="sm"
                              onClick={() => {
                                if (!requirement.serviceType) return
                                setSelectedServiceType(requirement.serviceType)
                                setIsCreateServiceDialogOpen(true)
                              }}
                            >
                              <Plus className="mr-2 h-4 w-4" />
                              Create {requirement.label}
                            </Button>
                          </div>
                        ) : (
                          <p className="mt-3 text-xs text-destructive">
                            No built-in creator is available for this service
                            type. Add a compatible service before continuing.
                          </p>
                        )}
                      </div>
                    ))}
                  </div>
                )}

              {availableServices.length > 0 && (
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  {availableServices.map((service: ExternalServiceInfo) => {
                    const isSelected = watchedServices.includes(service.id)
                    return (
                      <Card
                        key={service.id}
                        role="checkbox"
                        tabIndex={0}
                        aria-checked={isSelected}
                        className={cn(
                          'cursor-pointer transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
                          isSelected && 'ring-2 ring-primary'
                        )}
                        onClick={() => handleServiceToggle(service.id)}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter' || event.key === ' ') {
                            event.preventDefault()
                            handleServiceToggle(service.id)
                          }
                        }}
                      >
                        <CardHeader className="pb-3">
                          <div className="flex items-center gap-3">
                            <ServiceLogo service={service.service_type} />
                            <div>
                              <CardTitle className="text-sm">
                                {service.name}
                              </CardTitle>
                              <CardDescription className="text-xs">
                                {service.service_type} · Created{' '}
                                {format(
                                  new Date(service.created_at),
                                  'MMM d, yyyy'
                                )}
                              </CardDescription>
                            </div>
                          </div>
                        </CardHeader>
                      </Card>
                    )
                  })}
                </div>
              )}

              {!isLoadingServices &&
                !isServicesError &&
                availableServices.length === 0 &&
                serviceRequirements.length === 0 && (
                  <div className="text-center py-8">
                    <Database className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
                    <p className="text-sm text-muted-foreground">
                      No databases available
                    </p>
                    <p className="text-xs text-muted-foreground mt-1">
                      Create a database using the button above
                    </p>
                  </div>
                )}
            </CardContent>
          </Card>

          {/* Environment Variables */}
          <Card>
            <CardHeader>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="space-y-1.5">
                  <CardTitle>Environment Variables</CardTitle>
                  <CardDescription>
                    Review the template configuration and protect credentials as
                    write-only secrets.
                  </CardDescription>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={addEnvironmentVariable}
                >
                  <Plus className="h-4 w-4 mr-2" />
                  Add Variable
                </Button>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              <ProvidedEnvironmentVariables
                preset={template.preset}
                databases={watchedServices
                  .map((serviceId) =>
                    availableServices.find(
                      (service) => service.id === serviceId
                    )
                  )
                  .filter((service): service is ExternalServiceInfo =>
                    Boolean(service)
                  )
                  .map((service) => ({
                    id: service.id,
                    name: service.name,
                    serviceType: service.service_type,
                  }))}
                onVariablesChange={setProvidedEnvironmentVariables}
              />

              {missingRequiredVars.length > 0 && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    Missing required variables:{' '}
                    {missingRequiredVars.map((v) => v.name).join(', ')}
                  </AlertDescription>
                </Alert>
              )}

              {watchedEnvVars.length > 0 ? (
                <div className="space-y-3">
                  {watchedEnvVars.map((envVar, index) => {
                    const templateVar = configurableTemplateEnvVars.find(
                      (e) => e.name === envVar.key
                    )
                    return (
                      <Card
                        key={index}
                        className="border-border/70 bg-muted/15"
                      >
                        <CardContent className="p-4">
                          <div className="flex items-start gap-3">
                            <div className="flex-1 grid grid-cols-1 md:grid-cols-2 gap-3">
                              <FormField
                                control={form.control}
                                name={`environmentVariables.${index}.key`}
                                render={({ field }) => (
                                  <FormItem>
                                    <FormLabel className="text-sm flex items-center gap-2">
                                      Key
                                      {templateVar?.required && (
                                        <Badge
                                          variant="destructive"
                                          className="text-xs"
                                        >
                                          Required
                                        </Badge>
                                      )}
                                    </FormLabel>
                                    <FormControl>
                                      <Input
                                        {...field}
                                        placeholder="VARIABLE_NAME"
                                        readOnly={!!templateVar}
                                        className={cn(
                                          'font-mono',
                                          templateVar && 'bg-muted'
                                        )}
                                        autoCapitalize="none"
                                        autoCorrect="off"
                                        spellCheck={false}
                                      />
                                    </FormControl>
                                    {templateVar?.description && (
                                      <p className="text-xs text-muted-foreground">
                                        {templateVar.description}
                                      </p>
                                    )}
                                    <FormMessage />
                                    <ProvidedEnvironmentVariableWarning
                                      variableName={envVar.key}
                                      providedVariables={
                                        providedEnvironmentVariables
                                      }
                                    />
                                  </FormItem>
                                )}
                              />
                              <FormField
                                control={form.control}
                                name={`environmentVariables.${index}.value`}
                                render={({ field }) => {
                                  const generator =
                                    templateVar?.default_generator
                                  const handleGenerate = () => {
                                    const value = runGenerator(generator, {
                                      repositoryName:
                                        form.getValues('repositoryName'),
                                      base: deploymentUrlBase,
                                    })
                                    if (!value) {
                                      toast.error(
                                        generator === 'app_url'
                                          ? 'Enter a repository name first'
                                          : 'Could not generate value'
                                      )
                                      return
                                    }
                                    form.setValue(
                                      `environmentVariables.${index}.value`,
                                      value,
                                      {
                                        shouldValidate: true,
                                      }
                                    )
                                    setAutoGenerated((prev) => ({
                                      ...prev,
                                      [templateVar!.name]: value,
                                    }))
                                  }
                                  return (
                                    <FormItem>
                                      <FormLabel className="text-sm">
                                        Value
                                      </FormLabel>
                                      <div className="relative">
                                        <FormControl>
                                          <Input
                                            {...field}
                                            type={
                                              envVar.isSecret &&
                                              !showSecrets[index]
                                                ? 'password'
                                                : 'text'
                                            }
                                            placeholder={
                                              templateVar?.example ||
                                              'Enter value'
                                            }
                                            className={cn(
                                              'font-mono',
                                              generator &&
                                                envVar.isSecret &&
                                                'pr-20',
                                              generator &&
                                                !envVar.isSecret &&
                                                'pr-10',
                                              !generator &&
                                                envVar.isSecret &&
                                                'pr-10'
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
                                              envVar.isSecret
                                                ? 'right-9'
                                                : 'right-0'
                                            )}
                                            onClick={handleGenerate}
                                            title={
                                              generator === 'app_url'
                                                ? 'Generate from repository name'
                                                : 'Generate random value'
                                            }
                                          >
                                            <Sparkles className="h-4 w-4" />
                                          </Button>
                                        )}
                                        {envVar.isSecret && (
                                          <Button
                                            type="button"
                                            variant="ghost"
                                            size="sm"
                                            className="absolute right-0 top-0 h-full px-3"
                                            onClick={() =>
                                              setShowSecrets((prev) => ({
                                                ...prev,
                                                [index]: !prev[index],
                                              }))
                                            }
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
                                  )
                                }}
                              />
                              <FormField
                                control={form.control}
                                name={`environmentVariables.${index}.isSecret`}
                                render={({ field }) => (
                                  <FormItem className="flex items-start gap-3 space-y-0 rounded-md border bg-background/70 p-3 md:col-span-2">
                                    <FormControl>
                                      <Checkbox
                                        checked={field.value}
                                        onCheckedChange={field.onChange}
                                        className="mt-0.5"
                                      />
                                    </FormControl>
                                    <div className="space-y-1">
                                      <FormLabel className="text-sm">
                                        Encrypt as secret
                                      </FormLabel>
                                      <p className="text-xs text-muted-foreground">
                                        Secret values are write-only after
                                        creation. Use this for passwords,
                                        tokens, and private connection strings.
                                      </p>
                                    </div>
                                  </FormItem>
                                )}
                              />
                            </div>
                            {!templateVar && (
                              <Button
                                type="button"
                                variant="ghost"
                                size="sm"
                                onClick={() => removeEnvironmentVariable(index)}
                                className="text-destructive hover:text-destructive h-8 w-8 p-0 mt-6"
                                aria-label={`Remove environment variable ${index + 1}`}
                                title="Remove variable"
                              >
                                <X className="h-4 w-4" />
                              </Button>
                            )}
                          </div>
                        </CardContent>
                      </Card>
                    )
                  })}
                </div>
              ) : (
                <div className="text-center py-8">
                  <Settings className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
                  <p className="text-sm text-muted-foreground">
                    No environment variables configured
                  </p>
                  <p className="text-xs text-muted-foreground mt-1">
                    Add app configuration or credentials if this template needs
                    them
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="mt-4"
                    onClick={addEnvironmentVariable}
                  >
                    <Plus className="h-4 w-4 mr-2" />
                    Add your first variable
                  </Button>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Actions */}
          <div className="flex justify-end gap-3">
            {onCancel && (
              <Button
                type="button"
                variant="outline"
                onClick={onCancel}
                disabled={createFromTemplateMutation.isPending}
              >
                Cancel
              </Button>
            )}
            <Button
              type="submit"
              disabled={
                createFromTemplateMutation.isPending ||
                missingRequiredVars.length > 0 ||
                missingServiceRequirements.length > 0
              }
            >
              {createFromTemplateMutation.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Creating Project...
                </>
              ) : (
                <>
                  <CheckCircle2 className="mr-2 h-4 w-4" />
                  Create Project from Template
                </>
              )}
            </Button>
          </div>
        </form>
      </Form>

      {/* Create Service Dialog */}
      <CreateServiceDialog
        open={isCreateServiceDialogOpen && !!selectedServiceType}
        onOpenChange={(open) => {
          setIsCreateServiceDialogOpen(open)
          if (!open) setSelectedServiceType(null)
        }}
        serviceType={selectedServiceType || 'postgres'}
        onSuccess={(service: ExternalServiceInfo) => {
          setIsCreateServiceDialogOpen(false)
          setSelectedServiceType(null)
          setNewlyCreatedServices((previousServices) => {
            if (
              previousServices.some(
                (existingService) => existingService.id === service.id
              )
            ) {
              return previousServices
            }
            return [...previousServices, service]
          })
          const currentServices = form.getValues('storageServices') || []
          form.setValue(
            'storageServices',
            Array.from(new Set([...currentServices, service.id]))
          )
          void refetchServices()
          toast.success(`Database "${service.name}" created successfully!`)
        }}
      />
    </div>
  )
}

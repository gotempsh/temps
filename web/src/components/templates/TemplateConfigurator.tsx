import { useState, useEffect, useMemo, useCallback } from 'react'
import { useNavigate } from 'react-router-dom'
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
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  TemplateResponse,
  ConnectionResponse,
  ExternalServiceInfo,
  ServiceTypeRoute,
} from '@/api/client/types.gen'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
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
import { ServiceEnvPreview } from '@/components/project/ServiceEnvPreview'
import { TemplateImage } from '@/components/templates/TemplateImage'
import {
  runGenerator,
  generatorDependsOnRepoName,
  resolveDeploymentUrlBase,
} from '@/components/templates/envVarGenerators'
import { useSettings } from '@/hooks/useSettings'
import { getErrorMessage } from '@/utils/errorHandling'
import { cn } from '@/lib/utils'
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
  Gitlab,
  Loader2,
  Lock,
  Plus,
  Settings,
  Sparkles,
  Star,
  User,
  X,
} from 'lucide-react'
import Github from '@/icons/Github'

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

// Common service types
const SERVICE_TYPES = [
  { id: 'postgres' as ServiceTypeRoute, name: 'PostgreSQL', description: 'Reliable Relational Database' },
  { id: 'redis' as ServiceTypeRoute, name: 'Redis', description: 'In-Memory Data Store' },
  { id: 's3' as ServiceTypeRoute, name: 'S3 / RustFS', description: 'S3-compatible Object Storage' },
  { id: 'libsql' as ServiceTypeRoute, name: 'LibSQL', description: 'SQLite-compatible Database' },
]

// Form schema
const formSchema = z.object({
  projectName: z.string().min(1, 'Project name is required'),
  repositoryName: z.string().min(1, 'Repository name is required'),
  repositoryOwner: z.string().optional(),
  gitProviderConnectionId: z.number({ message: 'Git provider connection is required' }),
  private: z.boolean(),
  automaticDeploy: z.boolean(),
  storageServices: z.array(z.number()),
  environmentVariables: z.array(
    z.object({
      name: z.string().min(1, 'Variable name is required'),
      value: z.string(),
    })
  ),
})

type FormValues = z.infer<typeof formSchema>

// Repository URL Preview Component
interface RepositoryPreviewProps {
  repositoryName: string
  repositoryOwner?: string
  connection?: ConnectionResponse
}

function RepositoryPreview({ repositoryName, repositoryOwner, connection }: RepositoryPreviewProps) {
  if (!repositoryName || !connection) return null

  const owner = repositoryOwner || connection.account_name
  const repoUrl = `github.com/${owner}/${repositoryName}`

  return (
    <div className="rounded-lg border bg-muted/50 p-4">
      <div className="flex items-center gap-2 text-sm">
        <GitBranch className="h-4 w-4 text-muted-foreground" />
        <span className="text-muted-foreground">Repository will be created at:</span>
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
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] = useState(false)
  const [selectedServiceType, setSelectedServiceType] = useState<ServiceTypeRoute | null>(null)
  const [newlyCreatedServiceIds, setNewlyCreatedServiceIds] = useState<number[]>([])

  // Fetch connections
  const { data: connectionsData, isLoading: isLoadingConnections } = useQuery({
    ...listConnectionsOptions(),
  })

  // Fetch git providers so we can render the right icon per connection
  // (a GitLab connection should not show the GitHub mark).
  const { data: gitProviders } = useQuery({
    ...listGitProvidersOptions(),
  })

  const providerTypeForConnection = (conn: ConnectionResponse): string | undefined =>
    gitProviders?.find((p) => p.id === conn.provider_id)?.provider_type

  // Fetch existing services
  const { data: existingServices, refetch: refetchServices } = useQuery({
    ...listServicesOptions({}),
  })

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
      }),
    [platformSettings?.preview_domain, platformSettings?.external_url]
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
      environmentVariables: template.env_vars.map((env) => {
        const generated =
          runGenerator(env.default_generator, {
            repositoryName: initialRepoName,
            base: deploymentUrlBase,
          }) || ''
        return {
          name: env.name,
          value: env.default || generated,
        }
      }),
    },
  })

  // Track which generator-produced values are still "untouched" by the user so
  // we can re-run repo-name-dependent generators (`app_url`) when the slug changes.
  // Keyed by env-var name, value is the last value we generated.
  const [autoGenerated, setAutoGenerated] = useState<Record<string, string>>(() => {
    const seeded: Record<string, string> = {}
    for (const env of template.env_vars) {
      const value =
        runGenerator(env.default_generator, {
          repositoryName: initialRepoName,
          base: deploymentUrlBase,
        }) || ''
      if (value && !env.default) seeded[env.name] = value
    }
    return seeded
  })

  // Auto-select first connection when available
  useEffect(() => {
    if (connectionsData?.connections?.length && !form.getValues('gitProviderConnectionId')) {
      form.setValue('gitProviderConnectionId', connectionsData.connections[0].id, {
        shouldValidate: true,
      })
    }
  }, [connectionsData, form])

  // Watch project name to update repo name
  const projectName = useWatch({ control: form.control, name: 'projectName' })
  useEffect(() => {
    if (projectName) {
      form.setValue('repositoryName', generateRepoName(projectName), { shouldValidate: false })
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
  const repositoryNameWatch = useWatch({ control: form.control, name: 'repositoryName' })
  useEffect(() => {
    if (!repositoryNameWatch) return
    const currentVars = form.getValues('environmentVariables') || []
    const nextAutoGenerated = { ...autoGenerated }
    let mutated = false

    template.env_vars.forEach((envTemplate) => {
      if (!generatorDependsOnRepoName(envTemplate.default_generator)) return

      const idx = currentVars.findIndex((v) => v.name === envTemplate.name)
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
        form.setValue(`environmentVariables.${idx}.value`, newValue, { shouldValidate: false })
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
      // eslint-disable-next-line no-console
      console.error('Template project creation failed:', error)
    },
  })

  // Service toggle handler
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)
      form.setValue(
        'storageServices',
        isSelected ? currentServices.filter((id) => id !== serviceId) : [...currentServices, serviceId]
      )
    },
    [form]
  )

  // Form submission
  const handleSubmit = async (data: FormValues) => {
    // Combine existing and newly created services
    const allServiceIds = Array.from(
      new Set([...(data.storageServices || []), ...newlyCreatedServiceIds])
    )

    await createFromTemplateMutation.mutateAsync({
      body: {
        template_slug: template.slug,
        project_name: data.projectName,
        git_provider_connection_id: data.gitProviderConnectionId,
        repository_name: data.repositoryName,
        repository_owner: data.repositoryOwner || undefined,
        private: data.private,
        automatic_deploy: data.automaticDeploy,
        storage_service_ids: allServiceIds,
        environment_variables: data.environmentVariables
          .filter((env) => env.name && env.value)
          .map((env) => ({ name: env.name, value: env.value })),
      },
    })
  }

  // Add environment variable
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue('environmentVariables', [...currentVars, { name: '', value: '' }], {
      shouldValidate: false,
    })
  }

  // Remove environment variable
  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  const watchedServices = form.watch('storageServices') || []
  const watchedEnvVars = form.watch('environmentVariables') || []

  // Check if required env vars are filled
  const requiredEnvVars = template.env_vars.filter((e) => e.required)
  const missingRequiredVars = useMemo(() => {
    return requiredEnvVars.filter((required) => {
      const current = watchedEnvVars.find((e) => e.name === required.name)
      return !current?.value
    })
  }, [requiredEnvVars, watchedEnvVars])

  if (isLoadingConnections) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (!connectionsData?.connections?.length) {
    return (
      <Card className={className}>
        <CardContent className="py-12 text-center">
          <Github className="h-12 w-12 mx-auto text-muted-foreground mb-4" />
          <h3 className="font-semibold mb-2">No Git Provider Connected</h3>
          <p className="text-sm text-muted-foreground mb-4">
            You need to connect a Git provider to create projects from templates.
          </p>
          <Button onClick={() => navigate('/git-providers')}>Connect Git Provider</Button>
        </CardContent>
      </Card>
    )
  }

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
                  {template.is_featured && <Star className="h-4 w-4 text-yellow-500 fill-yellow-500" />}
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

              <FormField
                control={form.control}
                name="gitProviderConnectionId"
                render={({ field }) => {
                  const conns = connectionsData?.connections ?? []
                  const setValue = (id: number) => field.onChange(id)

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
                          <span className="font-medium">{only.account_name}</span>
                          <span className="text-xs text-muted-foreground">
                            ({only.account_type})
                          </span>
                        </div>
                        <FormDescription>
                          A new repository will be created in your connected Git account
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
                                  <RadioGroupItem id={id} value={conn.id.toString()} />
                                  <ProviderIcon
                                    providerType={providerTypeForConnection(conn)}
                                  />
                                  <span className="font-medium">{conn.account_name}</span>
                                  <span className="text-xs text-muted-foreground">
                                    ({conn.account_type})
                                  </span>
                                </Label>
                              )
                            })}
                          </RadioGroup>
                        </FormControl>
                        <FormDescription>
                          A new repository will be created in your selected Git account
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
                            <SelectItem key={conn.id} value={conn.id.toString()}>
                              <div className="flex items-center gap-2">
                                <ProviderIcon
                                  providerType={providerTypeForConnection(conn)}
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
                        A new repository will be created in your connected Git account
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )
                }}
              />

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
                      This will be the name of the new repository created from the template
                    </FormDescription>
                    <FormMessage />
                  </FormItem>
                )}
              />

              <FormField
                control={form.control}
                name="repositoryOwner"
                render={({ field }) => {
                  const selectedConnection = connectionsData?.connections?.find(
                    (c: ConnectionResponse) => c.id === form.watch('gitProviderConnectionId')
                  )
                  return (
                    <FormItem>
                      <FormLabel>Repository Owner</FormLabel>
                      <Select
                        value={field.value || '_personal'}
                        onValueChange={(v) => field.onChange(v === '_personal' ? undefined : v)}
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
                          {selectedConnection && selectedConnection.account_type === 'Organization' && (
                            <SelectItem value={selectedConnection.account_name}>
                              <div className="flex items-center gap-2">
                                <Building2 className="h-4 w-4" />
                                <span>{selectedConnection.account_name}</span>
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

              {/* Repository URL Preview */}
              <RepositoryPreview
                repositoryName={form.watch('repositoryName')}
                repositoryOwner={form.watch('repositoryOwner')}
                connection={connectionsData?.connections?.find(
                  (c: ConnectionResponse) => c.id === form.watch('gitProviderConnectionId')
                )}
              />

              <div className="flex flex-col gap-4 sm:flex-row">
                <FormField
                  control={form.control}
                  name="private"
                  render={({ field }) => (
                    <FormItem className="flex-1 flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
                      <FormControl>
                        <Checkbox checked={field.value} onCheckedChange={field.onChange} />
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
                        <Checkbox checked={field.value} onCheckedChange={field.onChange} />
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
            </CardContent>
          </Card>

          {/* Services */}
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Services</CardTitle>
                  <CardDescription>Link storage and database services</CardDescription>
                </div>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button type="button" variant="outline" size="sm">
                      <Plus className="h-4 w-4 mr-2" />
                      Add Service
                      <ChevronDown className="h-4 w-4 ml-1" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="w-[240px]">
                    {SERVICE_TYPES.map((type) => (
                      <DropdownMenuItem
                        key={type.id}
                        onClick={() => {
                          setSelectedServiceType(type.id)
                          setIsCreateServiceDialogOpen(true)
                        }}
                        className="flex items-start gap-3 py-3"
                      >
                        <ServiceLogo service={type.id} />
                        <div className="flex flex-col">
                          <span className="font-medium">{type.name}</span>
                          <span className="text-xs text-muted-foreground">{type.description}</span>
                        </div>
                      </DropdownMenuItem>
                    ))}
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            </CardHeader>
            <CardContent>
              {template.services.length > 0 && (
                <Alert className="mb-4">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    This template recommends: <strong>{template.services.join(', ')}</strong>.
                    Make sure to add these services for full functionality.
                  </AlertDescription>
                </Alert>
              )}

              {existingServices && existingServices.length > 0 && (
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                  {existingServices.map((service: ExternalServiceInfo) => {
                    const isSelected = watchedServices.includes(service.id)
                    return (
                      <Card
                        key={service.id}
                        className={cn(
                          'cursor-pointer transition-colors hover:bg-muted/50',
                          isSelected && 'ring-2 ring-primary'
                        )}
                        onClick={() => handleServiceToggle(service.id)}
                      >
                        <CardHeader className="pb-3">
                          <div className="flex items-center gap-3">
                            <ServiceLogo service={service.service_type} />
                            <div>
                              <CardTitle className="text-sm">{service.name}</CardTitle>
                              <CardDescription className="text-xs">
                                {service.service_type} · Created{' '}
                                {format(new Date(service.created_at), 'MMM d, yyyy')}
                              </CardDescription>
                            </div>
                          </div>
                        </CardHeader>
                      </Card>
                    )
                  })}
                </div>
              )}

              {(!existingServices || existingServices.length === 0) && (
                <div className="text-center py-8">
                  <Database className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
                  <p className="text-sm text-muted-foreground">No services available</p>
                  <p className="text-xs text-muted-foreground mt-1">
                    Create services using the button above
                  </p>
                </div>
              )}

              {watchedServices.length > 0 && existingServices && (
                <div className="mt-4 space-y-3">
                  <h4 className="text-sm font-medium">Selected Service Variables</h4>
                  {watchedServices.map((serviceId) => {
                    const service = existingServices.find((s: ExternalServiceInfo) => s.id === serviceId)
                    if (!service) return null
                    return (
                      <ServiceEnvPreview
                        key={service.id}
                        serviceId={service.id}
                        serviceName={service.name}
                        serviceType={service.service_type}
                      />
                    )
                  })}
                </div>
              )}
            </CardContent>
          </Card>

          {/* Environment Variables */}
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>Environment Variables</CardTitle>
                  <CardDescription>Configure required environment variables</CardDescription>
                </div>
                <Button type="button" variant="outline" size="sm" onClick={addEnvironmentVariable}>
                  <Plus className="h-4 w-4 mr-2" />
                  Add Variable
                </Button>
              </div>
            </CardHeader>
            <CardContent className="space-y-4">
              {missingRequiredVars.length > 0 && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    Missing required variables: {missingRequiredVars.map((v) => v.name).join(', ')}
                  </AlertDescription>
                </Alert>
              )}

              {watchedEnvVars.length > 0 ? (
                <div className="space-y-3">
                  {watchedEnvVars.map((envVar, index) => {
                    const templateVar = template.env_vars.find((e) => e.name === envVar.name)
                    return (
                      <Card key={index} className="border-dashed">
                        <CardContent className="p-4">
                          <div className="flex items-start gap-3">
                            <div className="flex-1 grid grid-cols-1 md:grid-cols-2 gap-3">
                              <FormField
                                control={form.control}
                                name={`environmentVariables.${index}.name`}
                                render={({ field }) => (
                                  <FormItem>
                                    <FormLabel className="text-sm flex items-center gap-2">
                                      Key
                                      {templateVar?.required && (
                                        <Badge variant="destructive" className="text-xs">
                                          Required
                                        </Badge>
                                      )}
                                    </FormLabel>
                                    <FormControl>
                                      <Input
                                        {...field}
                                        placeholder="VARIABLE_NAME"
                                        readOnly={!!templateVar}
                                        className={templateVar ? 'bg-muted' : ''}
                                      />
                                    </FormControl>
                                    {templateVar?.description && (
                                      <p className="text-xs text-muted-foreground">
                                        {templateVar.description}
                                      </p>
                                    )}
                                    <FormMessage />
                                  </FormItem>
                                )}
                              />
                              <FormField
                                control={form.control}
                                name={`environmentVariables.${index}.value`}
                                render={({ field }) => {
                                  const generator = templateVar?.default_generator
                                  const handleGenerate = () => {
                                    const value = runGenerator(generator, {
                                      repositoryName: form.getValues('repositoryName'),
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
                                    form.setValue(`environmentVariables.${index}.value`, value, {
                                      shouldValidate: true,
                                    })
                                    setAutoGenerated((prev) => ({
                                      ...prev,
                                      [templateVar!.name]: value,
                                    }))
                                  }
                                  return (
                                    <FormItem>
                                      <FormLabel className="text-sm">Value</FormLabel>
                                      <div className="relative">
                                        <FormControl>
                                          <Input
                                            {...field}
                                            type={showSecrets[index] ? 'text' : 'password'}
                                            placeholder={templateVar?.example || 'Enter value'}
                                            className={generator ? 'pr-20' : 'pr-10'}
                                          />
                                        </FormControl>
                                        {generator && (
                                          <Button
                                            type="button"
                                            variant="ghost"
                                            size="sm"
                                            className="absolute right-9 top-0 h-full px-2"
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
                                        >
                                          {showSecrets[index] ? (
                                            <EyeOff className="h-4 w-4" />
                                          ) : (
                                            <Eye className="h-4 w-4" />
                                          )}
                                        </Button>
                                      </div>
                                      <FormMessage />
                                    </FormItem>
                                  )
                                }}
                              />
                            </div>
                            {!templateVar && (
                              <Button
                                type="button"
                                variant="ghost"
                                size="sm"
                                onClick={() => removeEnvironmentVariable(index)}
                                className="text-destructive hover:text-destructive h-8 w-8 p-0 mt-6"
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
                  <p className="text-sm text-muted-foreground">No environment variables configured</p>
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
              disabled={createFromTemplateMutation.isPending || missingRequiredVars.length > 0}
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
          setNewlyCreatedServiceIds((prev) => [...prev, service.id])
          const currentServices = form.getValues('storageServices') || []
          form.setValue('storageServices', [...currentServices, service.id])
          setTimeout(() => refetchServices(), 100)
          toast.success(`Service "${service.name}" created successfully!`)
        }}
      />
    </div>
  )
}

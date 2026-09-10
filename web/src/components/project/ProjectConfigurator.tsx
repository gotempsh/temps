// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createProjectMutation,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  detectPublicPresetsOptions,
  getRepositoryEnvExampleLiveOptions,
  detectPublicEnvExampleOptions,
  getRepositoryComposeServicesLiveOptions,
  getPublicComposeServicesOptions,
  listPresetsOptions,
  revealServiceEnvironmentVariablesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { detectPublicPresets } from '@/api/client/sdk.gen'
import {
  CreatableServiceTypeRoute,
  RepositoryResponse,
  ProjectPresetResponse,
  BranchInfo,
  ExternalServiceInfo,
  EnvExampleVariableResponse,
} from '@/api/client/types.gen'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn, withMinDuration } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Database,
  ExternalLink,
  Eye,
  EyeOff,
  Folder,
  Globe,
  Info,
  Link2,
  Loader2,
  Plus,
  Settings,
  Sparkles,
  Upload,
  Wand2,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import * as z from 'zod/v4'
import {
  ProvidedEnvironmentVariables,
  ProvidedEnvironmentVariableWarning,
} from './ProvidedEnvironmentVariables'
import {
  isNonOverridableProvidedEnvironmentVariable,
  type ProvidedEnvironmentVariableCollision,
} from '@/lib/provided-environment-variables'
import { repositoryFilePath } from '@/lib/repository-file-path'
import { FrameworkSelector } from './FrameworkSelector'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { ADD_SERVICE_TYPES } from '@/lib/addServiceTypes'
import {
  generateSecretValue,
  getGeneratedSecretSpec,
  SECRET_LENGTH_OPTIONS,
} from '@/lib/secretGeneration'
import {
  generateAppUrl,
  resolveDeploymentUrlBase,
} from '@/components/templates/envVarGenerators'
import { useSettings } from '@/hooks/useSettings'
import {
  isLikelySecretProjectEnvironmentVariable,
  projectEnvironmentVariablesSchema,
} from '@/lib/project-environment-variables'
import {
  normalizeTemplateServiceType,
  toggleDatabaseSelection,
} from '@/lib/template-service-requirements'
import { useAllServices } from '@/hooks/useAllServices'
import { detectedPortForSelection } from '@/lib/dockerfile-port'

// Derives a browsable repo URL from whatever the API gave us. clone_url is an
// HTTPS URL (possibly `.git`-suffixed) for connected providers, but for the
// "continue with git URL" flow it's the raw string the user typed, which may
// be SSH shorthand (git@host:owner/repo) — normalize both to https://host/owner/repo.
function getRepositoryUrl(repository: RepositoryResponse): string | null {
  const raw = repository.clone_url || repository.ssh_url
  if (!raw) return null
  let url = raw.trim().replace(/\.git$/, '')
  const sshMatch = url.match(/^git@([^:]+):(.+)$/)
  if (sshMatch) {
    url = `https://${sshMatch[1]}/${sshMatch[2]}`
  }
  return url.startsWith('http://') || url.startsWith('https://') ? url : null
}

// Best-effort provider detection from the repo URL's hostname, for the brand
// logo next to the repo name. `RepositoryResponse` doesn't carry a provider
// type field directly (only `git_provider_connection_id`), so this mirrors
// the hostname-sniffing already used in BranchSelector.tsx's
// `detectProviderFromUrl`. Falls back to ProviderLogo's generic branch icon
// when the host doesn't match a known provider (e.g. self-hosted Gitea).
function detectProviderType(url: string | null): string | null {
  if (!url) return null
  const hostname = url.toLowerCase()
  if (hostname.includes('github')) return 'github'
  if (hostname.includes('gitlab')) return 'gitlab'
  if (hostname.includes('bitbucket')) return 'bitbucket'
  if (hostname.includes('gitea')) return 'gitea'
  return null
}

// Detected env vars shaped like a connection string (DATABASE_URL,
// POSTGRES_URL, MONGODB_URI, ...) can be filled from a service's real
// connection value instead of typed by hand.
function isConnectionStringKey(key: string): boolean {
  return /(URL|URI)$/i.test(key)
}

// A service's own basic env vars rarely use the exact name the repo's
// .env.example expects (a Postgres service exposes POSTGRES_URL, not
// DATABASE_URL) — so match the detected key exactly first, then fall back to
// whatever *_URL/*_URI the service does expose, preferring DATABASE_URL if
// it's among them since that's the most common convention.
function pickConnectionValue(
  vars: Record<string, string>,
  detectedKey: string
): string | null {
  if (vars[detectedKey]) return vars[detectedKey]
  const candidates = Object.entries(vars).filter(([key]) =>
    isConnectionStringKey(key)
  )
  if (candidates.length === 0) return null
  const preferred = candidates.find(([key]) => key === 'DATABASE_URL')
  return (preferred ?? candidates[0])[1]
}

// Helper function to normalize path for consistent comparison
// Normalizes '.', './', and empty strings to 'root'
function normalizePath(path: string | undefined | null): string {
  if (!path || path === '.' || path === './') {
    return 'root'
  }
  return path
}

// Helper function to slugify path for project name
const slugifyPath = (path: string): string => {
  if (!path || path === '.' || path === './' || path === 'root') {
    return ''
  }
  // Remove leading ./ if present
  const cleanPath = path.startsWith('./') ? path.slice(2) : path
  // Replace / with - and remove any other special characters
  return cleanPath.replace(/\//g, '-').replace(/[^a-zA-Z0-9-]/g, '')
}

// Form schema definition
const formSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  preset: z.string().min(1, 'Preset is required'),
  autoDeploy: z.boolean(),
  rootDirectory: z.string(),
  branch: z.string().min(1, 'Branch is required'),
  environmentVariables: projectEnvironmentVariablesSchema,
  storageServices: z.array(z.number()),
  dockerfilePath: z.string().optional(),
  composePath: z.string().optional(),
  excludedServices: z.array(z.string()).optional(),
  composeServices: z
    .array(
      z.object({
        name: z.string(),
        image: z.string().optional(),
        looksLikeDatabase: z.boolean(),
        detectedServiceType: z.string().nullable().optional(),
        ports: z
          .array(
            z.object({
              target: z.number(),
              published: z.number().optional(),
              protocol: z.string(),
            })
          )
          .optional(),
      })
    )
    .optional(),
  port: z.number().int().min(1).max(65535).optional(),
})

export type ProjectFormValues = z.infer<typeof formSchema>

// Compose file selector for docker-compose preset
function ComposeFileSelector({
  form,
  presetData,
  repositoryId,
  branch,
  publicRepo,
}: {
  form: ReturnType<typeof useForm<ProjectFormValues>>
  presetData:
    | {
        presets?: {
          preset: string
          compose_files?: string[] | null
          composeFiles?: string[] | null
        }[]
      }
    | undefined
    | null
  /** Real repository ID for connected repos (0/undefined for public "git URL" imports — those use `publicRepo` instead). */
  repositoryId: number | undefined
  branch: string | undefined
  /** Set for public "git URL" imports, where there's no `repositoryId` to key the live preview query on. */
  publicRepo?: {
    provider: string
    owner: string
    repo: string
    baseUrl?: string
  } | null
}) {
  const [isCustomPath, setIsCustomPath] = useState(false)
  const rootDirectory = form.watch('rootDirectory') || './'

  // Extract compose files from the docker-compose preset data,
  // filtered to only show files within the current root directory
  const composeFiles = useMemo(() => {
    if (!presetData?.presets) return []
    const composePreset = presetData.presets.find(
      (p) => p.preset === 'docker-compose'
    )
    // Handle both camelCase (from API) and snake_case
    const allFiles =
      composePreset?.composeFiles ?? composePreset?.compose_files ?? []

    // Normalize root directory: strip leading ./ and trailing /
    const normalizedRoot = rootDirectory.replace(/^\.\//, '').replace(/\/$/, '')

    if (!normalizedRoot) {
      // Root is repo root — show all files but use just the filename
      return allFiles.map((f) => ({
        full: f,
        relative: f,
      }))
    }

    // Filter to only files within the root directory, then make paths relative
    return allFiles
      .filter((f) => f.startsWith(normalizedRoot + '/'))
      .map((f) => ({
        full: f,
        relative: f.slice(normalizedRoot.length + 1),
      }))
  }, [presetData, rootDirectory])

  // Auto-select first compose file when files are discovered
  useEffect(() => {
    if (composeFiles.length > 0 && !form.getValues('composePath')) {
      form.setValue('composePath', composeFiles[0].relative)
    }
  }, [composeFiles, form])

  const composePath = form.watch('composePath')
  const composeRepositoryPath = repositoryFilePath(
    rootDirectory,
    composePath || ''
  )
  const excludedServices = form.watch('excludedServices') ?? []

  // Live-parses the selected compose file's services so the user can exclude
  // one before the project is even created (e.g. a raw `postgres` container,
  // which never gets Temps backup/restore, in favor of a managed database).
  // Two data sources, same split as the preset/env-example queries above:
  // `repositoryId` for connected repos, `publicRepo` for "git URL" imports.
  const {
    data: connectedComposeServicesData,
    isFetching: isLoadingConnectedComposeServices,
  } = useQuery({
    ...getRepositoryComposeServicesLiveOptions({
      path: { repository_id: repositoryId || 0 },
      query: { branch, path: composeRepositoryPath },
    }),
    enabled: !publicRepo && !!repositoryId && !!branch && !!composePath,
  })
  const {
    data: publicComposeServicesData,
    isFetching: isLoadingPublicComposeServices,
  } = useQuery({
    ...getPublicComposeServicesOptions({
      path: {
        provider: publicRepo?.provider || 'github',
        owner: publicRepo?.owner || '',
        repo: publicRepo?.repo || '',
      },
      query: {
        branch,
        path: composeRepositoryPath,
        base_url: publicRepo?.baseUrl,
      },
    }),
    enabled:
      !!publicRepo && !!publicRepo.owner && !!publicRepo.repo && !!composePath,
  })
  // The connected-repo response is camelCase (`looksLikeDatabase`); the
  // public-repo one is snake_case (`looks_like_database`) — same
  // inconsistency the codebase already has between base.rs and public.rs
  // handlers for env-example. Normalize to one shape here.
  const composeServices = useMemo(() => {
    const raw =
      connectedComposeServicesData?.services ??
      publicComposeServicesData?.services ??
      []
    return raw.map((s) => ({
      name: s.name,
      image: s.image ?? undefined,
      looksLikeDatabase:
        (s as { looksLikeDatabase?: boolean }).looksLikeDatabase ??
        (s as { looks_like_database?: boolean }).looks_like_database ??
        false,
      detectedServiceType:
        (s as { detectedServiceType?: string | null }).detectedServiceType ??
        (s as { detected_service_type?: string | null })
          .detected_service_type ??
        undefined,
      ports: (s.ports || []).map((port) => ({
        target: port.target,
        published: port.published ?? undefined,
        protocol: port.protocol ?? 'tcp',
      })),
    }))
  }, [connectedComposeServicesData, publicComposeServicesData])
  const isLoadingComposeServices =
    isLoadingConnectedComposeServices || isLoadingPublicComposeServices

  // Mirror the live-parsed list into the form so a freshly created project's
  // preset_config already has a populated checklist snapshot — no waiting on
  // the first deploy for the settings page to have anything to render.
  useEffect(() => {
    if (composeServices.length > 0) {
      form.setValue('composeServices', composeServices)
    }
  }, [composeServices, form])

  const toggleServiceIncluded = (serviceName: string, included: boolean) => {
    const current = form.getValues('excludedServices') ?? []
    form.setValue(
      'excludedServices',
      included
        ? current.filter((s) => s !== serviceName)
        : [...current, serviceName]
    )
  }

  const serviceChecklist = composeServices.length > 0 && (
    <div className="space-y-2 rounded-lg border p-3">
      <p className="text-sm font-medium">Services in this file</p>
      <TooltipProvider>
        <div className="space-y-1.5">
          {composeServices.map((service) => {
            const included = !excludedServices.includes(service.name)
            return (
              <div
                key={service.name}
                className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-muted/50"
              >
                <Checkbox
                  checked={included}
                  onCheckedChange={(checked) =>
                    toggleServiceIncluded(service.name, checked === true)
                  }
                  id={`compose-service-${service.name}`}
                />
                <label
                  htmlFor={`compose-service-${service.name}`}
                  className="flex flex-1 items-center gap-2 text-sm cursor-pointer"
                >
                  <span className="font-mono">{service.name}</span>
                  {service.image && (
                    <span className="text-xs text-muted-foreground font-mono">
                      {service.image}
                    </span>
                  )}
                </label>
                {service.looksLikeDatabase && (
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <span className="flex items-center gap-1 text-xs text-amber-600 dark:text-amber-500">
                        <Database className="h-3 w-3" />
                        <Info className="h-3 w-3" />
                      </span>
                    </TooltipTrigger>
                    <TooltipContent className="max-w-xs">
                      This looks like a database container — it won&apos;t have
                      Temps backup/restore. Consider excluding it and using a
                      Temps-managed database instead.
                    </TooltipContent>
                  </Tooltip>
                )}
              </div>
            )
          })}
        </div>
      </TooltipProvider>
      <p className="text-xs text-muted-foreground">
        Unchecked services are excluded from deployment entirely.
      </p>
    </div>
  )

  if (composeFiles.length === 0 || isCustomPath) {
    return (
      <div className="space-y-3">
        <FormField
          control={form.control}
          name="composePath"
          render={({ field }) => (
            <FormItem>
              <div className="flex items-center justify-between">
                <FormLabel>Compose File Path</FormLabel>
                {composeFiles.length > 0 && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setIsCustomPath(false)}
                    className="h-auto py-1 px-2 text-xs"
                  >
                    Back to detected files
                  </Button>
                )}
              </div>
              <FormControl>
                <Input
                  {...field}
                  placeholder="docker-compose.yml"
                  value={field.value || ''}
                />
              </FormControl>
              <p className="text-xs text-muted-foreground">
                Path to your compose file (e.g., docker-compose.yml,
                compose.yaml).
              </p>
              <FormMessage />
            </FormItem>
          )}
        />
        {isLoadingComposeServices && (
          <p className="text-xs text-muted-foreground flex items-center gap-1.5">
            <Loader2 className="h-3 w-3 animate-spin" />
            Reading services from compose file…
          </p>
        )}
        {serviceChecklist}
      </div>
    )
  }

  return (
    <div className="space-y-3">
      <FormField
        control={form.control}
        name="composePath"
        render={({ field }) => (
          <FormItem>
            <div className="flex items-center justify-between">
              <FormLabel>Compose File</FormLabel>
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={() => setIsCustomPath(true)}
                className="h-auto py-1 px-2 text-xs"
              >
                Enter custom path
              </Button>
            </div>
            <div className="space-y-2">
              {composeFiles.map((file) => (
                <div
                  key={file.full}
                  className={cn(
                    'flex items-center gap-3 p-3 rounded-lg border cursor-pointer transition-colors',
                    field.value === file.relative
                      ? 'border-primary bg-primary/5 ring-1 ring-primary'
                      : 'hover:bg-muted/50'
                  )}
                  onClick={() => field.onChange(file.relative)}
                >
                  <Folder className="h-4 w-4 text-muted-foreground shrink-0" />
                  <span className="font-mono text-sm">{file.relative}</span>
                </div>
              ))}
            </div>
            <p className="text-xs text-muted-foreground">
              {composeFiles.length} compose file
              {composeFiles.length !== 1 ? 's' : ''} found. Each service with
              exposed ports gets a subdomain automatically.
            </p>
            <FormMessage />
          </FormItem>
        )}
      />
      {isLoadingComposeServices && (
        <p className="text-xs text-muted-foreground flex items-center gap-1.5">
          <Loader2 className="h-3 w-3 animate-spin" />
          Reading services from compose file…
        </p>
      )}
      {serviceChecklist}
    </div>
  )
}

// Step definitions for different modes
type WizardStep = 'repo-config' | 'services' | 'env-vars' | 'review'

// Main component props
interface ProjectConfiguratorProps {
  // Repository data
  repository: RepositoryResponse
  connectionId?: number // Optional for public repos

  // Optional data
  branches?: BranchInfo[]
  /** Pre-loaded preset data for callers that already fetched it */
  presetData?: ProjectPresetResponse[]
  /**
   * Overrides the "Refresh" button's default behavior of refetching this
   * component's own preset query. Required whenever
   * `presetData` is supplied: that internal query is keyed on `repository.id`,
   * which is a real value only when the repo comes from `getRepositoryById`.
   * Callers that source presets another way must pass their own refetch here.
   */
  onRefreshPresets?: () => Promise<unknown>
  /**
   * Fallback env-example detection for callers that cannot identify either a
   * connected repository or a public repository.
   */
  envExampleData?: {
    path: string | null
    variables: EnvExampleVariableResponse[]
  }
  /**
   * Set for public "git URL" imports, so repository previews (env examples
   * and docker-compose services) query public endpoints instead of using the
   * synthetic `repository.id` from this flow.
   */
  publicRepo?: {
    provider: string
    owner: string
    repo: string
    baseUrl?: string
  } | null

  // Display modes
  mode?: 'wizard' | 'inline' | 'compact'

  // Behavior
  onSubmit?: (data: ProjectFormValues) => Promise<void>
  onCancel?: () => void
  showSteps?: boolean
  /**
   * Hide the repository summary card when the surrounding page already
   * displays the repo (e.g. the import page's context bar).
   */
  showRepositoryCard?: boolean
  defaultValues?: Partial<ProjectFormValues>

  // Styling
  className?: string
}

export function ProjectConfigurator({
  repository,
  connectionId,
  branches,
  presetData: providedPresetData,
  onRefreshPresets,
  envExampleData: providedEnvExampleData,
  publicRepo,
  mode: _mode = 'wizard',
  onSubmit,
  onCancel,
  showSteps: _showSteps = true,
  showRepositoryCard = true,
  defaultValues,
  className,
}: ProjectConfiguratorProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // State management
  const [_currentStep, _setCurrentStep] = useState<WizardStep>('repo-config')
  const [isSubmitting, setIsSubmitting] = useState(false)
  // Synchronous re-entry guard: React state updates are async, so a fast
  // click can fire handleSubmit again before `isSubmitting` flips. A ref
  // gives us a synchronous gate that prevents duplicate project creates
  // (root cause of users ending up with 15 identical projects).
  const isSubmittingRef = useRef(false)
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<CreatableServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [isImportEnvOpen, setIsImportEnvOpen] = useState(false)
  const [providedEnvironmentVariables, setProvidedEnvironmentVariables] =
    useState<ProvidedEnvironmentVariableCollision[] | null>(null)
  const [newlyCreatedServices, setNewlyCreatedServices] = useState<
    ExternalServiceInfo[]
  >([])
  const [allowDirectoryOverride, setAllowDirectoryOverride] = useState(false)

  // Form initialization
  const form = useForm<ProjectFormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit', // Only validate on submit to avoid early validation errors
    defaultValues: {
      name: defaultValues?.name ?? repository?.name ?? '',
      preset: defaultValues?.preset ?? '', // Start empty, will be auto-filled by useEffect
      autoDeploy: defaultValues?.autoDeploy ?? true,
      rootDirectory: defaultValues?.rootDirectory ?? './',
      branch: defaultValues?.branch ?? repository?.default_branch ?? 'main',
      environmentVariables: defaultValues?.environmentVariables ?? [],
      storageServices: defaultValues?.storageServices ?? [],
      dockerfilePath: defaultValues?.dockerfilePath ?? 'Dockerfile',
      excludedServices: defaultValues?.excludedServices ?? [],
      composeServices: defaultValues?.composeServices ?? [],
      port: defaultValues?.port ?? 3000,
    },
  })

  // Fetch existing services
  const {
    data: existingServices,
    isPending: areServicesPending,
    isError: didServicesFail,
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

  // Platform settings provide `preview_domain` / `external_url`, which drive
  // the suggested "this project's URL" option offered for *_URL vars in the
  // env-var fill-from pickers below — same generator TemplateConfigurator
  // uses for e.g. NEXTAUTH_URL, so a repo's own URL vars get a real,
  // routable value instead of a guess.
  const { data: platformSettings } = useSettings()
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
  const projectNameValue = useWatch({ control: form.control, name: 'name' })
  const suggestedAppUrl = useMemo(() => {
    // The backend slugifies the project name into `project.slug` at create
    // time, then assigns the first environment the subdomain
    // `{project.slug}-{env.slug}` — mirror that slugification here so the
    // preview URL matches what actually gets deployed.
    const slug = projectNameValue
      ?.trim()
      .toLowerCase()
      .replace(/[^a-z0-9-]/g, '-')
      .replace(/-+/g, '-')
      .replace(/^-|-$/g, '')
    return generateAppUrl({
      repositoryName: slug ?? '',
      base: deploymentUrlBase,
    })
  }, [projectNameValue, deploymentUrlBase])

  // Fetch all available presets to get default ports
  const { data: allPresetsData } = useQuery({
    ...listPresetsOptions({}),
  })

  // Fetch branches if not provided (only for authenticated connections)
  const { data: branchesData } = useQuery({
    ...getRepositoryBranchesOptions({
      query: { connection_id: connectionId! },
      path: { owner: repository.owner || '', repo: repository.name || '' },
    }),
    enabled:
      !branches && !!connectionId && !!repository.owner && !!repository.name,
  })

  const effectiveBranches = useMemo(
    () => branches || branchesData?.branches || [],
    [branches, branchesData?.branches]
  )

  // Watch the selected branch to refetch presets when it changes
  const selectedBranch = useWatch({
    control: form.control,
    name: 'branch',
  })
  const selectedRootDirectory = useWatch({
    control: form.control,
    name: 'rootDirectory',
  })

  // Fetch connected preset data (will refetch when branch changes due to query key).
  const {
    data: fetchedPresetData,
    isLoading: connectedPresetLoading,
    isFetching: connectedPresetFetching,
    error: connectedPresetError,
    refetch: refetchPresets,
  } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repository.id || 0 },
      query: { branch: selectedBranch },
    }),
    enabled:
      !providedPresetData && !publicRepo && !!repository.id && !!selectedBranch,
    // Key includes branch, so React Query will refetch when branch changes
  })

  const publicPresetQueryOptions = detectPublicPresetsOptions({
    path: {
      provider: publicRepo?.provider || 'github',
      owner: publicRepo?.owner || '',
      repo: publicRepo?.repo || '',
    },
    query: {
      branch: selectedBranch,
      base_url: publicRepo?.baseUrl,
    },
  })
  const {
    data: fetchedPublicPresetData,
    isLoading: publicPresetLoading,
    isFetching: publicPresetFetching,
    error: publicPresetError,
  } = useQuery({
    ...publicPresetQueryOptions,
    enabled: !providedPresetData && !!publicRepo && !!selectedBranch,
  })

  const presetLoading = publicRepo
    ? publicPresetLoading
    : connectedPresetLoading
  const presetFetching = publicRepo
    ? publicPresetFetching
    : connectedPresetFetching
  const presetError = publicRepo ? publicPresetError : connectedPresetError

  // Holds the manual "Refresh" click for a minimum visible duration so a
  // fast response doesn't cut the spin icon off before it completes a turn.
  const [isManuallyRefreshingPresets, setIsManuallyRefreshingPresets] =
    useState(false)
  const handleRefreshPresets = async () => {
    setIsManuallyRefreshingPresets(true)
    try {
      await withMinDuration(async () => {
        if (onRefreshPresets) {
          await onRefreshPresets()
          return
        }
        if (publicRepo) {
          const response = await detectPublicPresets({
            path: {
              provider: publicRepo.provider,
              owner: publicRepo.owner,
              repo: publicRepo.repo,
            },
            query: {
              branch: selectedBranch,
              base_url: publicRepo.baseUrl,
              fresh: true,
            },
            throwOnError: true,
          })
          queryClient.setQueryData(
            publicPresetQueryOptions.queryKey,
            response.data
          )
          return
        }
        await refetchPresets()
      })
    } finally {
      setIsManuallyRefreshingPresets(false)
    }
  }

  // Use provided presets or fetched presets
  // Transform providedPresetData to match the expected structure { presets: [...] }
  const presetData = useMemo(() => {
    if (providedPresetData) {
      return { presets: providedPresetData }
    }
    if (fetchedPublicPresetData) {
      return {
        presets: fetchedPublicPresetData.presets.map((preset) => ({
          preset: preset.preset,
          presetLabel: preset.preset_label,
          exposedPort: preset.exposed_port,
          iconUrl: preset.icon_url,
          projectType: preset.project_type,
          path: preset.path,
          composeFiles: preset.compose_files,
          dockerfilePath: preset.dockerfile_path,
        })),
      }
    }
    return fetchedPresetData
  }, [providedPresetData, fetchedPublicPresetData, fetchedPresetData])

  // Detect a .env.example (or common variant) directly inside the selected
  // project root. Including the root directory in the query key makes preset
  // and manual-directory changes replace the detected variables.
  const {
    data: fetchedEnvExampleData,
    isFetching: isFetchingConnectedEnvExample,
  } = useQuery({
    ...getRepositoryEnvExampleLiveOptions({
      path: { repository_id: repository.id || 0 },
      query: {
        branch: selectedBranch,
        root_directory: selectedRootDirectory || './',
      },
    }),
    enabled: !publicRepo && !!repository.id && !!selectedBranch,
  })

  const {
    data: fetchedPublicEnvExampleData,
    isFetching: isFetchingPublicEnvExample,
  } = useQuery({
    ...detectPublicEnvExampleOptions({
      path: {
        provider: publicRepo?.provider || 'github',
        owner: publicRepo?.owner || '',
        repo: publicRepo?.repo || '',
      },
      query: {
        branch: selectedBranch,
        root_directory: selectedRootDirectory || './',
        base_url: publicRepo?.baseUrl,
      },
    }),
    enabled: !!publicRepo && !!selectedBranch,
  })

  const isFetchingEnvExample = publicRepo
    ? isFetchingPublicEnvExample
    : isFetchingConnectedEnvExample
  const envExamplePath = isFetchingEnvExample
    ? null
    : fetchedEnvExampleData
      ? fetchedEnvExampleData.path
      : fetchedPublicEnvExampleData
        ? fetchedPublicEnvExampleData.path
        : (providedEnvExampleData?.path ?? null)
  const detectedEnvExampleVariables = useMemo(() => {
    if (isFetchingEnvExample) return []
    if (fetchedEnvExampleData) return fetchedEnvExampleData.variables
    if (fetchedPublicEnvExampleData) {
      return fetchedPublicEnvExampleData.variables.map((variable) => ({
        key: variable.key,
        defaultValue: variable.default_value,
        description: variable.description,
      }))
    }
    return providedEnvExampleData?.variables ?? []
  }, [
    providedEnvExampleData,
    fetchedEnvExampleData,
    fetchedPublicEnvExampleData,
    isFetchingEnvExample,
  ])
  const envExampleVariables = useMemo(
    () =>
      detectedEnvExampleVariables.filter(
        (variable) =>
          !isNonOverridableProvidedEnvironmentVariable(
            variable.key,
            providedEnvironmentVariables ?? []
          )
      ),
    [detectedEnvExampleVariables, providedEnvironmentVariables]
  )

  const [envExampleDismissed, setEnvExampleDismissed] = useState(false)
  const [selectedEnvExampleKeys, setSelectedEnvExampleKeys] = useState<
    Set<string>
  >(new Set())
  const [envExampleValueDrafts, setEnvExampleValueDrafts] = useState<
    Record<string, string>
  >({})
  // Tracks which detected var is currently fetching a service's real
  // connection value, so only that row's picker shows a spinner.
  const [fillingEnvExampleKey, setFillingEnvExampleKey] = useState<
    string | null
  >(null)
  // Same as above, but for the manually-added environmentVariables rows
  // below the detected card — tracked by row index since those rows have no
  // stable key of their own.
  const [fillingManualVarIndex, setFillingManualVarIndex] = useState<
    number | null
  >(null)

  // Re-seed selection/drafts whenever a *new* detection result comes in
  // (new array reference), without clobbering edits the user made to the
  // current one on every re-render.
  useEffect(() => {
    setEnvExampleDismissed(false)
    setSelectedEnvExampleKeys(new Set(envExampleVariables.map((v) => v.key)))
    setEnvExampleValueDrafts(
      Object.fromEntries(
        envExampleVariables.map((v) => [v.key, v.defaultValue])
      )
    )
  }, [envExamplePath, envExampleVariables, selectedRootDirectory])

  // Default project creation mutation
  const projectMutation = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success('Project created successfully!')
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  // Compute the default preset value based on preset data
  const defaultPresetValue = useMemo(() => {
    if (!presetData) {
      return null
    }

    // New schema: use presets array
    if (presetData.presets && presetData.presets.length > 0) {
      const firstPreset = presetData.presets[0]
      const presetName = firstPreset.preset || 'custom'
      const presetPath = firstPreset.path || './'
      const normalizedPath = normalizePath(presetPath)
      return {
        value: `${presetName}::${normalizedPath}`,
        rootDir: presetPath, // Keep original path for rootDirectory field
        // Carries the rolled-up Dockerfile path (see onSelectPreset) so the
        // very first auto-selected preset pre-fills it too, not just
        // subsequent manual selections.
        dockerfilePath:
          presetName === 'dockerfile'
            ? firstPreset.dockerfilePath || undefined
            : undefined,
      }
    }
    // Fallback: use 'custom' as default
    else {
      return {
        value: 'custom',
        rootDir: './',
      }
    }
  }, [presetData])

  // Auto-set preset when data is available (select first available preset)
  useEffect(() => {
    // Only run if we have a default preset value
    if (!defaultPresetValue) {
      return
    }

    const currentPreset = form.getValues('preset')
    // Only auto-set if preset is empty
    if (currentPreset) {
      return
    }

    // Store the preset value in format "preset::path"
    form.setValue('preset', defaultPresetValue.value, {
      shouldValidate: true,
      shouldDirty: true,
    })
    form.setValue('rootDirectory', defaultPresetValue.rootDir, {
      shouldValidate: true,
      shouldDirty: true,
    })
    if (defaultPresetValue.dockerfilePath) {
      form.setValue('dockerfilePath', defaultPresetValue.dockerfilePath, {
        shouldValidate: true,
        shouldDirty: true,
      })
    }
  }, [defaultPresetValue, form])

  // Auto-set default branch (select first branch by default)
  useEffect(() => {
    const currentBranch = form.getValues('branch')

    // Only set if we don't have a branch yet
    if (!currentBranch && effectiveBranches && effectiveBranches.length > 0) {
      // Try to find default branch (marked as default or matching repo's default branch)
      const defaultBranch = effectiveBranches.find(
        (b: BranchInfo) => b.name === repository.default_branch
      )

      // Fallback to first branch if no default found
      const selectedBranch = defaultBranch || effectiveBranches[0]

      if (selectedBranch) {
        form.setValue('branch', selectedBranch.name, {
          shouldValidate: true,
          shouldDirty: true,
        })
      }
    }
  }, [effectiveBranches, repository.default_branch, form])

  // Watch preset changes to update port automatically
  const selectedPreset = useWatch({
    control: form.control,
    name: 'preset',
  })
  const selectedPort = useWatch({
    control: form.control,
    name: 'port',
  })
  const selectedDockerfilePath = useWatch({
    control: form.control,
    name: 'dockerfilePath',
  })
  const detectedPresetPort = useMemo(
    () => detectedPortForSelection(presetData?.presets, selectedPreset),
    [presetData?.presets, selectedPreset]
  )
  const selectedPresetName = selectedPreset?.split('::')[0]?.toLowerCase()
  const effectiveDetectedPort =
    selectedPresetName === 'dockerfile' &&
    (selectedDockerfilePath || 'Dockerfile') === 'Dockerfile'
      ? detectedPresetPort
      : undefined
  const hasDockerfilePortMismatch =
    selectedPresetName === 'dockerfile' &&
    effectiveDetectedPort !== undefined &&
    selectedPort !== undefined &&
    selectedPort !== effectiveDetectedPort
  const portWasManuallyEdited = useRef(false)

  // Auto-update port based on selected preset
  useEffect(() => {
    if (!selectedPreset) {
      return
    }

    if (portWasManuallyEdited.current) {
      return
    }

    if (effectiveDetectedPort !== undefined) {
      form.setValue('port', effectiveDetectedPort, {
        shouldValidate: true,
        shouldDirty: false,
      })
      return
    }

    if (!allPresetsData?.presets) return

    // Extract preset name from "preset::path" format
    const [presetName] = selectedPreset.split('::')

    // Find the matching preset to get its default port
    const matchingPreset = allPresetsData.presets.find(
      (p) => p.slug === presetName
    )

    if (
      matchingPreset &&
      matchingPreset.default_port !== null &&
      matchingPreset.default_port !== undefined &&
      matchingPreset.default_port > 0
    ) {
      form.setValue('port', matchingPreset.default_port, {
        shouldValidate: true,
        shouldDirty: false,
      })
    }
  }, [selectedPreset, effectiveDetectedPort, allPresetsData, form])

  // Environment variable management
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      [...currentVars, { key: '', value: '', isSecret: false }],
      { shouldValidate: false }
    ) // Don't validate when adding empty variables
  }

  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  const addSelectedEnvExampleVariables = () => {
    const currentVars = form.getValues('environmentVariables') || []
    const existingKeys = new Set(currentVars.map((v) => v.key).filter(Boolean))
    const newVars = envExampleVariables
      .filter(
        (v) => selectedEnvExampleKeys.has(v.key) && !existingKeys.has(v.key)
      )
      .map((v) => ({
        key: v.key,
        value: envExampleValueDrafts[v.key] ?? v.defaultValue,
        isSecret: isLikelySecretProjectEnvironmentVariable(v.key),
      }))
    if (newVars.length === 0) return
    form.setValue('environmentVariables', [...currentVars, ...newVars])
    setEnvExampleDismissed(true)
  }

  const toggleSelectAllEnvExampleVars = () => {
    setSelectedEnvExampleKeys((prev) =>
      prev.size === envExampleVariables.length
        ? new Set()
        : new Set(envExampleVariables.map((v) => v.key))
    )
  }

  const fillEnvExampleValueFromService = async (
    varKey: string,
    service: ExternalServiceInfo
  ) => {
    setFillingEnvExampleKey(varKey)
    try {
      const vars = await queryClient.fetchQuery(
        revealServiceEnvironmentVariablesOptions({ path: { id: service.id } })
      )
      const value = pickConnectionValue(vars ?? {}, varKey)
      if (!value) {
        toast.error(`"${service.name}" doesn't expose a connection URL`)
        return
      }
      setEnvExampleValueDrafts((prev) => ({ ...prev, [varKey]: value }))
      setSelectedEnvExampleKeys((prev) => new Set(prev).add(varKey))
    } catch {
      toast.error(`Failed to read environment variables from "${service.name}"`)
    } finally {
      setFillingEnvExampleKey(null)
    }
  }

  const generateEnvExampleSecret = (varKey: string, bytes: number) => {
    const spec = getGeneratedSecretSpec(varKey)
    if (!spec) return
    const value = generateSecretValue({ ...spec, bytes })
    setEnvExampleValueDrafts((prev) => ({ ...prev, [varKey]: value }))
    setSelectedEnvExampleKeys((prev) => new Set(prev).add(varKey))
  }

  const fillEnvExampleValueWithAppUrl = (varKey: string) => {
    if (!suggestedAppUrl) return
    setEnvExampleValueDrafts((prev) => ({ ...prev, [varKey]: suggestedAppUrl }))
    setSelectedEnvExampleKeys((prev) => new Set(prev).add(varKey))
  }

  const fillManualEnvVarFromService = async (
    index: number,
    service: ExternalServiceInfo
  ) => {
    const varKey = form.getValues(`environmentVariables.${index}.key`)
    setFillingManualVarIndex(index)
    try {
      const vars = await queryClient.fetchQuery(
        revealServiceEnvironmentVariablesOptions({ path: { id: service.id } })
      )
      const value = pickConnectionValue(vars ?? {}, varKey)
      if (!value) {
        toast.error(`"${service.name}" doesn't expose a connection URL`)
        return
      }
      form.setValue(`environmentVariables.${index}.value`, value)
      form.setValue(`environmentVariables.${index}.isSecret`, true)
    } catch {
      toast.error(`Failed to read environment variables from "${service.name}"`)
    } finally {
      setFillingManualVarIndex(null)
    }
  }

  const fillManualEnvVarWithAppUrl = (index: number) => {
    if (!suggestedAppUrl) return
    form.setValue(`environmentVariables.${index}.value`, suggestedAppUrl)
  }

  // Service selection handler
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

  // Handle form submission
  const handleSubmit = async (data: ProjectFormValues) => {
    if (isSubmittingRef.current) return
    isSubmittingRef.current = true
    try {
      if (providedEnvironmentVariables === null) {
        toast.error('Wait for the provided environment variables to load.')
        return
      }
      const blockedIndex = data.environmentVariables.findIndex((variable) =>
        isNonOverridableProvidedEnvironmentVariable(
          variable.key,
          providedEnvironmentVariables
        )
      )
      if (blockedIndex >= 0) {
        form.setError(`environmentVariables.${blockedIndex}.key`, {
          message: 'Temps provides this variable automatically at deployment',
        })
        toast.error('Remove the environment variable managed by Temps.')
        return
      }
      setIsSubmitting(true)

      // Extract just the preset name from "preset::path" format for backend
      const [presetName] = data.preset.split('::')

      const finalData = {
        ...data,
        preset: presetName, // Use only the preset name, not the full "preset::path"
        // The visible selection is authoritative. A newly created database
        // can be deselected before project submission.
        storageServices: Array.from(new Set(data.storageServices || [])),
      }

      if (onSubmit) {
        await onSubmit(finalData)
      } else {
        // Use default mutation
        await projectMutation.mutateAsync({
          body: {
            name: finalData.name,
            preset: finalData.preset,
            directory: finalData.rootDirectory,
            main_branch: finalData.branch,
            repo_name: repository.name || '',
            repo_owner: repository.owner || '',
            git_url: repository.clone_url || repository.ssh_url || undefined,
            git_provider_connection_id: connectionId,
            project_type:
              finalData.preset === 'custom' ? 'static' : finalData.preset,
            automatic_deploy: finalData.autoDeploy,
            storage_service_ids: finalData.storageServices || [],
            environment_variables: finalData.environmentVariables?.map(
              (env) => ({
                key: env.key,
                value: env.value,
                is_secret: env.isSecret,
              })
            ),
            preset_config:
              finalData.preset === 'dockerfile' && finalData.dockerfilePath
                ? {
                    dockerfilePath: finalData.dockerfilePath,
                  }
                : finalData.preset === 'docker-compose'
                  ? {
                      composePath:
                        finalData.composePath || 'docker-compose.yml',
                      ...(finalData.excludedServices &&
                      finalData.excludedServices.length > 0
                        ? { excludedServices: finalData.excludedServices }
                        : {}),
                      ...(finalData.composeServices &&
                      finalData.composeServices.length > 0
                        ? { composeServices: finalData.composeServices }
                        : {}),
                    }
                  : undefined,
          },
        })
      }
    } catch (error) {
      console.error('Project configuration error:', error)
    } finally {
      isSubmittingRef.current = false
      setIsSubmitting(false)
    }
  }

  // Render repository config step. The repository summary card is skipped
  // when the surrounding page already shows the repo (e.g. the import page's
  // context bar) so the name isn't printed twice.
  const repositoryUrl = getRepositoryUrl(repository)
  const repositoryProviderType = detectProviderType(repositoryUrl)

  const renderRepoConfig = () => (
    <div className="space-y-4">
      {showRepositoryCard && (
        <div className="flex items-center gap-2 px-3 py-2 bg-muted/50 rounded-md text-sm">
          <ProviderLogo
            providerType={repositoryProviderType}
            className="h-4 w-4 shrink-0 text-muted-foreground"
          />
          {repositoryUrl ? (
            <a
              href={repositoryUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="font-medium hover:underline inline-flex items-center gap-1.5 truncate"
            >
              {repository.full_name}
              <ExternalLink className="h-3 w-3 text-muted-foreground shrink-0" />
            </a>
          ) : (
            <div className="font-medium truncate">{repository.full_name}</div>
          )}
        </div>
      )}

      {/* Name and branch share one row — neither needs full width, and the
          preset grid below is the part that deserves the vertical space. */}
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
        <FormField
          control={form.control}
          name="name"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Project Name</FormLabel>
              <FormControl>
                <Input {...field} placeholder="Enter project name" />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />

        <FormField
          control={form.control}
          name="branch"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Branch</FormLabel>
              <BranchSelector
                repoOwner={repository.owner}
                repoName={repository.name}
                connectionId={connectionId}
                defaultBranch={repository.default_branch}
                value={field.value}
                onChange={field.onChange}
              />
              <FormMessage />
            </FormItem>
          )}
        />
      </div>

      <FormField
        control={form.control}
        name="preset"
        render={({ field }) => {
          // Convert stored preset value back to select format for display
          const getSelectValue = () => {
            if (field.value === 'custom') return 'custom'
            if (!field.value) return ''

            // Find matching preset to get the path (detected presets)
            const matchingPreset = presetData?.presets?.find(
              (p: ProjectPresetResponse) => p.preset === field.value
            )
            if (matchingPreset) {
              return `${matchingPreset.preset}::${matchingPreset.path || 'root'}`
            }

            // If no match found in detected presets, return just the preset slug
            // (This happens when selecting from "Browse all presets" mode)
            return field.value
          }

          const selectValue = getSelectValue()

          return (
            <FormItem>
              <FormControl>
                <FrameworkSelector
                  presetData={presetData}
                  isLoading={presetLoading}
                  isRefreshing={presetFetching || isManuallyRefreshingPresets}
                  error={presetError}
                  selectedPreset={selectValue}
                  onRefresh={handleRefreshPresets}
                  onSelectPreset={(value) => {
                    if (value === 'custom') {
                      field.onChange('custom')
                      form.setValue('rootDirectory', './')
                    } else {
                      const [presetName, presetPath] = value.split('::')
                      // Store the full preset key (preset::path) to distinguish between same preset at different paths
                      field.onChange(value)

                      // Set project name based on repository name and preset path
                      const repoName = repository.name || 'project'
                      const slugifiedPath = slugifyPath(presetPath)
                      const projectName = slugifiedPath
                        ? `${repoName}-${slugifiedPath}`
                        : repoName
                      form.setValue('name', projectName)

                      // Treat empty, '.', and 'root' as root directory
                      if (
                        presetPath &&
                        presetPath !== 'root' &&
                        presetPath !== '.' &&
                        presetPath.trim() !== ''
                      ) {
                        // Remove leading ./ if present in the path
                        const cleanPath = presetPath.startsWith('./')
                          ? presetPath.slice(2)
                          : presetPath
                        form.setValue('rootDirectory', `./${cleanPath}`)
                      } else {
                        form.setValue('rootDirectory', './')
                      }

                      // Pre-fill the Dockerfile path from the detected
                      // candidate. Covers the rolled-up case — a Dockerfile
                      // that actually lives in a subdirectory (e.g.
                      // `docker/Dockerfile`) but is presented at the
                      // repository root so the build context stays there —
                      // and resets to the plain default otherwise, so a
                      // stale value from a previous selection never lingers.
                      if (presetName === 'dockerfile') {
                        const matchedCandidate = presetData?.presets?.find(
                          (p: ProjectPresetResponse) =>
                            p.preset === presetName &&
                            normalizePath(p.path) === normalizePath(presetPath)
                        )
                        form.setValue(
                          'dockerfilePath',
                          matchedCandidate?.dockerfilePath || 'Dockerfile'
                        )
                      }
                    }
                  }}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )
        }}
      />

      <FormField
        control={form.control}
        name="rootDirectory"
        render={({ field }) => {
          const currentPreset = form.watch('preset')
          const isCustomPreset = currentPreset === 'custom'
          const canEditDirectory = isCustomPreset || allowDirectoryOverride

          return (
            <FormItem>
              <div className="flex items-center justify-between">
                <FormLabel>Root Directory</FormLabel>
                {!isCustomPreset && !allowDirectoryOverride && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setAllowDirectoryOverride(true)}
                    className="h-auto py-1 px-2 text-xs"
                  >
                    Edit manually
                  </Button>
                )}
                {!isCustomPreset && allowDirectoryOverride && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setAllowDirectoryOverride(false)
                      // Reset to preset-based directory if available
                      const presetValue = form.getValues('preset')
                      if (presetValue && presetValue !== 'custom') {
                        const [, presetPath] = presetValue.split('::')
                        if (presetPath && presetPath !== 'root') {
                          const cleanPath = presetPath.startsWith('./')
                            ? presetPath.slice(2)
                            : presetPath
                          form.setValue('rootDirectory', `./${cleanPath}`)
                        } else {
                          form.setValue('rootDirectory', './')
                        }
                      }
                    }}
                    className="h-auto py-1 px-2 text-xs"
                  >
                    Reset to preset
                  </Button>
                )}
              </div>
              <FormControl>
                <Input
                  {...field}
                  placeholder="./"
                  readOnly={!canEditDirectory}
                  className={!canEditDirectory ? 'bg-muted' : ''}
                />
              </FormControl>
              <p className="text-xs text-muted-foreground">
                {canEditDirectory
                  ? 'Enter the root directory for your configuration'
                  : 'Directory will be set based on the selected framework preset'}
              </p>
              <FormMessage />
            </FormItem>
          )
        }}
      />

      <FormField
        control={form.control}
        name="autoDeploy"
        render={({ field }) => (
          <FormItem className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
            <FormControl>
              <Checkbox
                checked={field.value}
                onCheckedChange={field.onChange}
              />
            </FormControl>
            <div className="space-y-1 leading-none">
              <FormLabel>Automatic Deployments</FormLabel>
              <p className="text-sm text-muted-foreground">
                Automatically deploy when changes are pushed to the repository
              </p>
            </div>
          </FormItem>
        )}
      />

      {/* Docker Configuration - Only show for docker/dockerfile preset */}
      {form.watch('preset')?.split('::')[0]?.toLowerCase() === 'dockerfile' && (
        <FormField
          control={form.control}
          name="dockerfilePath"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Dockerfile Path</FormLabel>
              <FormControl>
                <Input
                  {...field}
                  placeholder="Dockerfile"
                  value={field.value || 'Dockerfile'}
                />
              </FormControl>
              <p className="text-xs text-muted-foreground">
                Path to your Dockerfile relative to the root directory
              </p>
              <FormMessage />
            </FormItem>
          )}
        />
      )}

      {/* Docker Compose Configuration */}
      {form.watch('preset')?.split('::')[0]?.toLowerCase() ===
        'docker-compose' && (
        <ComposeFileSelector
          form={form}
          presetData={presetData}
          repositoryId={repository.id}
          branch={selectedBranch}
          publicRepo={publicRepo}
        />
      )}

      {/* Application Port - hide for docker-compose (multiple services have their own ports) */}
      {form.watch('preset')?.split('::')[0]?.toLowerCase() !==
        'docker-compose' && (
        <FormField
          control={form.control}
          name="port"
          render={({ field }) => (
            <FormItem>
              <FormLabel>Application Port</FormLabel>
              <FormControl>
                <Input
                  type="number"
                  min="1"
                  max="65535"
                  placeholder="3000"
                  name={field.name}
                  ref={field.ref}
                  onBlur={field.onBlur}
                  value={field.value ?? 3000}
                  onChange={(e) => {
                    portWasManuallyEdited.current = true
                    const v = e.target.valueAsNumber
                    field.onChange(Number.isNaN(v) ? undefined : v)
                  }}
                />
              </FormControl>
              <p className="text-xs text-muted-foreground">
                {effectiveDetectedPort !== undefined
                  ? `Detected from EXPOSE ${effectiveDetectedPort} in this Dockerfile.`
                  : 'Port your application will listen on (e.g., 3000, 8080)'}
              </p>
              {hasDockerfilePortMismatch && (
                <Alert variant="destructive" className="mt-2">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    This Dockerfile exposes port {effectiveDetectedPort}, but
                    the project is configured for port {selectedPort}. Temps
                    routes to the exposed image port after the build, so an app
                    that listens on the configured PORT value may fail its
                    health check. Make these ports match unless your startup
                    command intentionally ignores PORT.
                  </AlertDescription>
                </Alert>
              )}
              <FormMessage />
            </FormItem>
          )}
        />
      )}
    </div>
  )

  const renderAddDatabaseMenu = () => (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={areServicesPending}
        >
          <Plus className="h-4 w-4 mr-2" />
          Add Database
          <ChevronDown className="h-4 w-4 ml-1" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        {ADD_SERVICE_TYPES.map((type) => {
          const selectedIds = form.getValues('storageServices') || []
          const isTypeAlreadySelected = availableServices.some(
            (service) =>
              selectedIds.includes(service.id) &&
              normalizeTemplateServiceType(service.service_type) === type.id
          )
          return (
            <DropdownMenuItem
              key={type.id}
              onClick={() => {
                if (isTypeAlreadySelected) {
                  toast.error(`A ${type.name} database is already selected`, {
                    description:
                      'Deselect it before creating another database of this type.',
                  })
                  return
                }
                setSelectedServiceType(type.id)
                setIsCreateServiceDialogOpen(true)
              }}
              className={cn(
                'flex items-center gap-3 py-2.5',
                isTypeAlreadySelected && 'cursor-not-allowed opacity-50'
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
  )

  // Render databases step. The API still calls these storage services, but
  // "Databases" is the user-facing concept in project creation.
  const renderDatabases = () => {
    const watchedServices = form.watch('storageServices') || []

    return (
      <div className="space-y-4">
        {areServicesPending && (
          <div
            className="grid grid-cols-1 gap-3 md:grid-cols-2"
            aria-label="Loading databases"
          >
            <Skeleton className="h-20 w-full rounded-lg" />
            <Skeleton className="h-20 w-full rounded-lg" />
          </div>
        )}

        {didServicesFail && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription className="flex items-center justify-between gap-3">
              <span>Databases could not be loaded.</span>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => void refetchServices()}
              >
                Try again
              </Button>
            </AlertDescription>
          </Alert>
        )}

        {availableServices.length > 0 && (
          <div>
            <h4 className="text-sm font-medium mb-3">Existing Databases</h4>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              {availableServices.map((service) => {
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
                      <div className="flex items-center justify-between">
                        <div className="flex items-center gap-3">
                          <ServiceLogo service={service.service_type} />
                          <div>
                            <CardTitle className="text-sm">
                              {service.name}
                            </CardTitle>
                            <CardDescription className="text-xs">
                              {service.service_type} • Created{' '}
                              {format(
                                new Date(service.created_at),
                                'MMM d, yyyy'
                              )}
                            </CardDescription>
                          </div>
                        </div>
                      </div>
                    </CardHeader>
                  </Card>
                )
              })}
            </div>
          </div>
        )}

        {newlyCreatedServices.some((service) =>
          watchedServices.includes(service.id)
        ) && (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              {
                newlyCreatedServices.filter((service) =>
                  watchedServices.includes(service.id)
                ).length
              }{' '}
              new database
              {newlyCreatedServices.filter((service) =>
                watchedServices.includes(service.id)
              ).length > 1
                ? 's'
                : ''}{' '}
              will be linked to this project
            </AlertDescription>
          </Alert>
        )}

        {!areServicesPending &&
          !didServicesFail &&
          availableServices.length === 0 && (
            <div className="text-center py-8">
              <Database className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
              <p className="text-sm text-muted-foreground">
                No databases configured yet
              </p>
              <p className="text-xs text-muted-foreground mt-1">
                Add PostgreSQL, Redis, MongoDB, or object storage when your app
                needs it
              </p>
            </div>
          )}
      </div>
    )
  }

  // Render environment variables step
  const renderEnvVars = () => {
    const watchedEnvVars = form.watch('environmentVariables') || []
    const selectedDatabases = (form.watch('storageServices') || [])
      .map((serviceId) =>
        availableServices.find((service) => service.id === serviceId)
      )
      .filter((service): service is ExternalServiceInfo => Boolean(service))
      .map((service) => ({
        id: service.id,
        name: service.name,
        serviceType: service.service_type,
      }))

    return (
      <div className="space-y-4">
        <ProvidedEnvironmentVariables
          preset={selectedPreset || 'dockerfile'}
          databases={selectedDatabases}
          onVariablesChange={setProvidedEnvironmentVariables}
        />

        {envExampleVariables.length > 0 && !envExampleDismissed && (
          <Card className="border-primary/30 bg-primary/5">
            <CardContent className="p-4 space-y-3">
              <div className="flex items-start justify-between gap-2">
                <div className="flex items-start gap-2">
                  <Sparkles className="h-4 w-4 text-primary mt-0.5 shrink-0" />
                  <div>
                    <p className="text-sm font-medium">
                      Detected {envExampleVariables.length} environment variable
                      {envExampleVariables.length === 1 ? '' : 's'}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      Found in{' '}
                      <span className="font-mono">{envExamplePath}</span> —
                      review and fill in the values below
                    </p>
                  </div>
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="h-6 px-2 text-xs"
                    onClick={toggleSelectAllEnvExampleVars}
                  >
                    {selectedEnvExampleKeys.size === envExampleVariables.length
                      ? 'Deselect all'
                      : 'Select all'}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="h-6 w-6 p-0"
                    onClick={() => setEnvExampleDismissed(true)}
                  >
                    <X className="h-3.5 w-3.5" />
                  </Button>
                </div>
              </div>

              <div className="space-y-2 max-h-64 overflow-y-auto pr-1">
                {envExampleVariables.map((v) => (
                  <div key={v.key} className="flex items-start gap-2">
                    <Checkbox
                      className="mt-2.5"
                      checked={selectedEnvExampleKeys.has(v.key)}
                      onCheckedChange={(checked) => {
                        setSelectedEnvExampleKeys((prev) => {
                          const next = new Set(prev)
                          if (checked) {
                            next.add(v.key)
                          } else {
                            next.delete(v.key)
                          }
                          return next
                        })
                      }}
                    />
                    <div className="flex-1 min-w-0 grid grid-cols-1 md:grid-cols-2 gap-1.5">
                      <div className="flex flex-col justify-center min-w-0">
                        <span className="font-mono text-sm truncate">
                          {v.key}
                        </span>
                        {v.description && (
                          <span className="text-xs text-muted-foreground line-clamp-3">
                            {v.description}
                          </span>
                        )}
                      </div>
                      <div className="flex items-center gap-1">
                        <Input
                          value={envExampleValueDrafts[v.key] ?? ''}
                          onChange={(e) => {
                            const { value } = e.target
                            setEnvExampleValueDrafts((prev) => ({
                              ...prev,
                              [v.key]: value,
                            }))
                            // Editing a value is a clear signal the user wants
                            // this variable included — don't make them also
                            // remember to check the box.
                            if (value) {
                              setSelectedEnvExampleKeys((prev) =>
                                prev.has(v.key)
                                  ? prev
                                  : new Set(prev).add(v.key)
                              )
                            }
                          }}
                          placeholder="Enter value"
                          className="font-mono text-sm h-8"
                        />
                        {getGeneratedSecretSpec(v.key) && (
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button
                                type="button"
                                variant="outline"
                                size="sm"
                                className="h-8 w-8 p-0 shrink-0"
                                title="Generate a random secret"
                              >
                                <Wand2 className="h-3.5 w-3.5" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end" className="w-64">
                              {SECRET_LENGTH_OPTIONS.map((option) => (
                                <DropdownMenuItem
                                  key={option.bytes}
                                  onClick={() =>
                                    generateEnvExampleSecret(
                                      v.key,
                                      option.bytes
                                    )
                                  }
                                  className="flex flex-col items-start gap-0.5"
                                >
                                  <span className="font-medium">
                                    {option.label} ({option.bytes} bytes)
                                  </span>
                                  <span className="text-xs text-muted-foreground">
                                    {option.description}
                                  </span>
                                </DropdownMenuItem>
                              ))}
                            </DropdownMenuContent>
                          </DropdownMenu>
                        )}
                        {isConnectionStringKey(v.key) && (
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button
                                type="button"
                                variant="outline"
                                size="sm"
                                className="h-8 w-8 p-0 shrink-0"
                                title="Fill from a database connection URL"
                                disabled={
                                  fillingEnvExampleKey === v.key ||
                                  (availableServices.length === 0 &&
                                    !suggestedAppUrl)
                                }
                              >
                                {fillingEnvExampleKey === v.key ? (
                                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                                ) : (
                                  <Link2 className="h-3.5 w-3.5" />
                                )}
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end" className="w-64">
                              {suggestedAppUrl && (
                                <>
                                  <DropdownMenuItem
                                    onClick={() =>
                                      fillEnvExampleValueWithAppUrl(v.key)
                                    }
                                    className="flex items-center gap-2"
                                  >
                                    <Globe className="h-4 w-4 shrink-0 text-muted-foreground" />
                                    <div className="flex flex-col min-w-0">
                                      <span>This project&apos;s URL</span>
                                      <span className="text-xs text-muted-foreground truncate">
                                        {suggestedAppUrl}
                                      </span>
                                    </div>
                                  </DropdownMenuItem>
                                  {availableServices.length > 0 && (
                                    <DropdownMenuSeparator />
                                  )}
                                </>
                              )}
                              {availableServices.length ? (
                                availableServices.map((service) => (
                                  <DropdownMenuItem
                                    key={service.id}
                                    onClick={() =>
                                      fillEnvExampleValueFromService(
                                        v.key,
                                        service
                                      )
                                    }
                                    className="flex items-center gap-2"
                                  >
                                    <ServiceLogo
                                      service={service.service_type}
                                      className="h-4 w-4 shrink-0"
                                    />
                                    <span className="truncate">
                                      {service.name}
                                    </span>
                                  </DropdownMenuItem>
                                ))
                              ) : !suggestedAppUrl ? (
                                <div className="px-2 py-1.5 text-xs text-muted-foreground">
                                  No databases yet — add one in the Databases
                                  section first
                                </div>
                              ) : null}
                            </DropdownMenuContent>
                          </DropdownMenu>
                        )}
                      </div>
                    </div>
                  </div>
                ))}
              </div>

              <Button
                type="button"
                size="sm"
                onClick={addSelectedEnvExampleVariables}
                disabled={selectedEnvExampleKeys.size === 0}
              >
                Add {selectedEnvExampleKeys.size} selected variable
                {selectedEnvExampleKeys.size === 1 ? '' : 's'}
              </Button>
            </CardContent>
          </Card>
        )}

        <ImportEnvDialog
          isOpen={isImportEnvOpen}
          onOpenChange={setIsImportEnvOpen}
          existingKeys={
            new Set(watchedEnvVars.map((v) => v.key).filter(Boolean))
          }
          showEnvironmentSelection={false}
          onImport={async (variables) => {
            const currentVars = form.getValues('environmentVariables') || []
            const configurableVariables = variables.filter(
              (variable) =>
                !isNonOverridableProvidedEnvironmentVariable(
                  variable.key,
                  providedEnvironmentVariables ?? []
                )
            )
            const skippedCount = variables.length - configurableVariables.length
            const newVars = configurableVariables.map((v) => ({
              key: v.key,
              value: v.value,
              isSecret: isLikelySecretProjectEnvironmentVariable(v.key),
            }))
            form.setValue('environmentVariables', [...currentVars, ...newVars])
            if (skippedCount > 0) {
              toast.info(
                `Skipped ${skippedCount} variable${skippedCount === 1 ? '' : 's'} provided automatically by Temps`
              )
            }
          }}
        />

        {watchedEnvVars.length > 0 ? (
          <div className="space-y-3">
            {watchedEnvVars.map((_, index) => (
              <Card key={index} className="border-border/70 bg-muted/15">
                <CardContent className="space-y-4 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <Badge variant="outline" className="font-normal">
                      Variable {index + 1}
                    </Badge>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => removeEnvironmentVariable(index)}
                      className="text-destructive hover:text-destructive h-8 w-8 p-0"
                      aria-label={`Remove environment variable ${index + 1}`}
                      title="Remove variable"
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  </div>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.key`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-sm">Key</FormLabel>
                          <FormControl>
                            <Input
                              {...field}
                              className="font-mono"
                              placeholder="DATABASE_URL"
                              autoCapitalize="none"
                              autoCorrect="off"
                              spellCheck={false}
                            />
                          </FormControl>
                          <FormMessage />
                          <ProvidedEnvironmentVariableWarning
                            variableName={watchedEnvVars[index]?.key ?? ''}
                            providedVariables={
                              providedEnvironmentVariables ?? []
                            }
                          />
                        </FormItem>
                      )}
                    />
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.value`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-sm">Value</FormLabel>
                          <div className="flex items-center gap-1">
                            <FormControl>
                              <Input
                                {...field}
                                type={
                                  watchedEnvVars[index]?.isSecret &&
                                  !showSecrets[index]
                                    ? 'password'
                                    : 'text'
                                }
                                className="font-mono"
                                placeholder="Enter value"
                              />
                            </FormControl>
                            {getGeneratedSecretSpec(
                              watchedEnvVars[index]?.key ?? ''
                            ) && (
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <Button
                                    type="button"
                                    variant="outline"
                                    size="sm"
                                    className="h-9 w-9 shrink-0 p-0"
                                    title="Generate a random secret"
                                    aria-label="Generate a random secret"
                                  >
                                    <Wand2 className="h-4 w-4" />
                                  </Button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent
                                  align="end"
                                  className="w-64"
                                >
                                  {SECRET_LENGTH_OPTIONS.map((option) => (
                                    <DropdownMenuItem
                                      key={option.bytes}
                                      onClick={() => {
                                        const spec = getGeneratedSecretSpec(
                                          watchedEnvVars[index]?.key ?? ''
                                        )
                                        if (!spec) return
                                        form.setValue(
                                          `environmentVariables.${index}.value`,
                                          generateSecretValue({
                                            ...spec,
                                            bytes: option.bytes,
                                          })
                                        )
                                        form.setValue(
                                          `environmentVariables.${index}.isSecret`,
                                          true
                                        )
                                      }}
                                      className="flex flex-col items-start gap-0.5"
                                    >
                                      <span className="font-medium">
                                        {option.label} ({option.bytes} bytes)
                                      </span>
                                      <span className="text-xs text-muted-foreground">
                                        {option.description}
                                      </span>
                                    </DropdownMenuItem>
                                  ))}
                                </DropdownMenuContent>
                              </DropdownMenu>
                            )}
                            {isConnectionStringKey(
                              watchedEnvVars[index]?.key ?? ''
                            ) &&
                              (availableServices.length > 0 ||
                                !!suggestedAppUrl) && (
                                <DropdownMenu>
                                  <DropdownMenuTrigger asChild>
                                    <Button
                                      type="button"
                                      variant="outline"
                                      size="sm"
                                      className="h-9 w-9 shrink-0 p-0"
                                      title="Fill from a database connection URL"
                                      aria-label="Fill from a database connection URL"
                                      disabled={fillingManualVarIndex === index}
                                    >
                                      {fillingManualVarIndex === index ? (
                                        <Loader2 className="h-4 w-4 animate-spin" />
                                      ) : (
                                        <Link2 className="h-4 w-4" />
                                      )}
                                    </Button>
                                  </DropdownMenuTrigger>
                                  <DropdownMenuContent
                                    align="end"
                                    className="w-64"
                                  >
                                    {suggestedAppUrl && (
                                      <>
                                        <DropdownMenuItem
                                          onClick={() =>
                                            fillManualEnvVarWithAppUrl(index)
                                          }
                                          className="flex items-center gap-2"
                                        >
                                          <Globe className="h-4 w-4 shrink-0 text-muted-foreground" />
                                          <div className="flex flex-col min-w-0">
                                            <span>This project&apos;s URL</span>
                                            <span className="text-xs text-muted-foreground truncate">
                                              {suggestedAppUrl}
                                            </span>
                                          </div>
                                        </DropdownMenuItem>
                                        {availableServices.length > 0 && (
                                          <DropdownMenuSeparator />
                                        )}
                                      </>
                                    )}
                                    {availableServices.map((service) => (
                                      <DropdownMenuItem
                                        key={service.id}
                                        onClick={() =>
                                          fillManualEnvVarFromService(
                                            index,
                                            service
                                          )
                                        }
                                        className="flex items-center gap-2"
                                      >
                                        <ServiceLogo
                                          service={service.service_type}
                                          className="h-4 w-4 shrink-0"
                                        />
                                        <span className="truncate">
                                          {service.name}
                                        </span>
                                      </DropdownMenuItem>
                                    ))}
                                  </DropdownMenuContent>
                                </DropdownMenu>
                              )}
                            {watchedEnvVars[index]?.isSecret && (
                              <Button
                                type="button"
                                variant="outline"
                                size="sm"
                                className="h-9 w-9 shrink-0 p-0"
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
                      )}
                    />
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.isSecret`}
                      render={({ field }) => (
                        <FormItem className="flex items-start gap-3 space-y-0 rounded-md border bg-background/70 p-3 md:col-span-2">
                          <FormControl>
                            <Checkbox
                              id={`env-var-secret-${index}`}
                              checked={field.value}
                              onCheckedChange={field.onChange}
                              className="mt-0.5"
                            />
                          </FormControl>
                          <div className="space-y-1 leading-none">
                            <FormLabel
                              htmlFor={`env-var-secret-${index}`}
                              className="text-sm font-medium"
                            >
                              Treat as secret
                            </FormLabel>
                            <p className="text-muted-foreground text-xs">
                              All values are encrypted at rest. Enable this for
                              stricter masking, permission checks, and audited
                              reveals.
                            </p>
                          </div>
                        </FormItem>
                      )}
                    />
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        ) : (
          <div className="text-center py-8">
            <Settings className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
            <p className="text-sm text-muted-foreground">
              No environment variables configured
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              Add one manually, import an existing .env file, or use the
              variables detected from your repository
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
      </div>
    )
  }

  // Render inline/compact mode
  return (
    <div className={cn('space-y-6', className)}>
      <Form {...form}>
        <form
          onSubmit={form.handleSubmit(handleSubmit, (errors) => {
            console.error('Form validation errors:', errors)
          })}
          className="space-y-6"
        >
          {/* All sections in one view for inline/compact mode */}
          <Card>
            <CardHeader>
              <CardTitle>Project Configuration</CardTitle>
              <CardDescription>Configure your project settings</CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {renderRepoConfig()}
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="space-y-1.5">
                <CardTitle>Databases</CardTitle>
                <CardDescription>
                  Link a managed database or storage resource. Connection
                  variables are injected automatically.
                </CardDescription>
              </div>
              {renderAddDatabaseMenu()}
            </CardHeader>
            <CardContent>{renderDatabases()}</CardContent>
          </Card>

          <Card>
            <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="space-y-1.5">
                <CardTitle>Environment Variables</CardTitle>
                <CardDescription>
                  Add app configuration and credentials. Database connection
                  variables above are included automatically.
                </CardDescription>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => setIsImportEnvOpen(true)}
                >
                  <Upload className="h-4 w-4 mr-2" />
                  Import .env
                </Button>
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
            <CardContent>{renderEnvVars()}</CardContent>
          </Card>

          <div className="flex justify-end gap-3">
            {onCancel && (
              <Button
                type="button"
                variant="outline"
                onClick={onCancel}
                disabled={isSubmitting}
              >
                Cancel
              </Button>
            )}
            <Button type="submit" disabled={isSubmitting}>
              {isSubmitting ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Creating Project...
                </>
              ) : (
                <>
                  <CheckCircle2 className="mr-2 h-4 w-4" />
                  Create Project
                </>
              )}
            </Button>
          </div>
        </form>
      </Form>

      {/* Create Service Dialog */}
      {selectedServiceType && (
        <CreateServiceDialog
          open={isCreateServiceDialogOpen}
          onOpenChange={(open) => {
            setIsCreateServiceDialogOpen(open)
            if (!open) {
              setSelectedServiceType(null)
            }
          }}
          serviceType={selectedServiceType}
          successMessage={(service) =>
            `Database "${service.name}" created successfully!`
          }
          onSuccess={(service: ExternalServiceInfo) => {
            setIsCreateServiceDialogOpen(false)
            setSelectedServiceType(null)
            setNewlyCreatedServices((previousServices) =>
              previousServices.some((item) => item.id === service.id)
                ? previousServices
                : [...previousServices, service]
            )
            // Automatically add the newly created service to the form selection
            const currentServices = form.getValues('storageServices') || []
            form.setValue(
              'storageServices',
              Array.from(new Set([...currentServices, service.id]))
            )
            void refetchServices()
          }}
        />
      )}
    </div>
  )
}

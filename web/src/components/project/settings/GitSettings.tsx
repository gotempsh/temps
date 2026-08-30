// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ProjectResponse,
  RepositoryResponse,
  getRepositoryBranches,
  listRepositoriesByConnection,
} from '@/api/client'
import {
  detectPublicPresetsOptions,
  getPublicBranchesOptions,
  getPublicRepositoryOptions,
  getRepositoryByIdOptions,
  getRepositoryPresetLiveOptions,
  getRepositoryComposeServicesLiveOptions,
  getPublicComposeServicesOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
  reinstallGitlabWebhookMutation,
  updateAutomaticDeployMutation,
  updateGitSettingsMutation,
  updateProjectSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RepositorySelector } from '@/components/repositories/RepositorySelector'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { Badge } from '@/components/ui/badge'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
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
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { useDebounce } from '@/hooks/useDebounce'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import { cn, withMinDuration } from '@/lib/utils'
import {
  findComposePortMapping,
  mergeComposeServicePorts,
  parseComposeOverridePorts,
  type ComposeServicePorts,
} from '@/lib/compose-port-discovery'
import {
  composePreviewErrorMessage,
  fetchComposePreview,
  isPublicRepositoryRateLimitError,
} from '@/lib/compose-preview'
import { composeSettingPatch } from '@/lib/compose-settings-patch'
import { withComposeSandboxDisabled } from '@/lib/compose-security-settings'
import {
  normalizePresetPath,
  presetConfigForSelection,
  presetSelectionKey,
  splitPresetSelection,
} from '@/lib/preset-selection'
import { repositoryFilePath } from '@/lib/repository-file-path'
import {
  projectSettingsSections,
  type ProjectSettingsView,
} from '@/lib/project-settings-view'
import {
  parseRepositoryCoordinates,
  parsePublicRepositoryUrl,
  publicRepositoryProvider,
} from '@/lib/public-repository'
import { useMutation, useQuery } from '@tanstack/react-query'
import Editor from '@monaco-editor/react'
import {
  Check,
  ChevronDown,
  ChevronsUpDown,
  Database,
  AlertTriangle,
  EyeOff,
  FileIcon,
  FolderIcon,
  GitBranchIcon,
  Info,
  Loader2,
  Plus,
  RefreshCw,
  Route,
  ShieldCheck,
  ShieldOff,
  Trash2,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useTheme } from 'next-themes'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import FrameworkIcon from '../FrameworkIcon'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { FrameworkSelector } from '../FrameworkSelector'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import {
  repositoryConnectionBasePath,
  repositoryConnectionPath,
  repositorySelectionPath,
} from '@/lib/repository-connection-route'

interface GitSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

interface GitSettingsInlineProps extends GitSettingsProps {
  view: ProjectSettingsView
  /**
   * Rendered inside a tab that already supplies the page heading, and beside a
   * Source tab that owns the deployment source. Suppresses this component's own
   * heading and its deployment-source summary so neither appears twice.
   */
  embedded?: boolean
}

function GitSettingsInline({
  project,
  refetch,
  view,
  embedded,
}: GitSettingsInlineProps) {
  const { resolvedTheme } = useTheme()
  const composeEditorTheme = resolvedTheme === 'dark' ? 'vs-dark' : 'light'
  const updateGitSettings = useMutation({
    ...updateGitSettingsMutation(),
    meta: { errorTitle: 'Failed to update git settings' },
  })
  const updateProjectSettings = useMutation({
    ...updateProjectSettingsMutation(),
    meta: { errorTitle: 'Failed to update project settings' },
  })
  const updateAutomaticDeploy = useMutation({
    ...updateAutomaticDeployMutation(),
    meta: { errorTitle: 'Failed to update auto-deploy' },
  })

  // ---------------- Live API data ----------------
  const isPublicRepo = project.is_public_repo
  const isUploadedSource = project.source_type === 'uploaded_source'
  const sections = projectSettingsSections(view, project.source_type)
  const publicProvider = publicRepositoryProvider(project?.git_url)
  const publicRepository = parsePublicRepositoryUrl(project?.git_url)

  const { data: providersData } = useQuery({ ...listGitProvidersOptions() })
  const providers = providersData || []

  const { data: connectionsData } = useQuery({ ...listConnectionsOptions() })
  const currentConnection = connectionsData?.connections?.find(
    (c) => c.id === project?.git_provider_connection_id
  )
  const currentProvider = providers.find(
    (p) => p.id === currentConnection?.provider_id
  )

  const {
    data: branchesData,
    isLoading: isLoadingConnectedBranches,
    refetch: refetchConnectedBranches,
  } = useQuery({
    queryKey: [
      'repository-branches',
      project?.repo_owner,
      project?.repo_name,
      project?.git_provider_connection_id,
    ],
    queryFn: async () => {
      if (
        !project?.repo_owner ||
        !project?.repo_name ||
        !project?.git_provider_connection_id
      ) {
        return {
          branches: [] as Array<{
            name: string
            commit_sha: string
            protected: boolean
          }>,
        }
      }
      const response = await getRepositoryBranches({
        path: { owner: project.repo_owner, repo: project.repo_name },
        query: { connection_id: project.git_provider_connection_id },
      })
      return response.data || { branches: [] }
    },
    enabled:
      !!project?.repo_owner &&
      !!project?.repo_name &&
      !!project?.git_provider_connection_id,
  })
  const publicBranchesQuery = useQuery({
    ...getPublicBranchesOptions({
      path: {
        provider: publicProvider,
        owner: project?.repo_owner || '',
        repo: project?.repo_name || '',
      },
      query: { base_url: publicRepository?.instanceUrl },
    }),
    enabled: isPublicRepo && !!project?.repo_owner && !!project?.repo_name,
    retry: false,
  })
  const branches: Array<{
    name: string
    commit_sha: string
    protected: boolean
  }> =
    ((isPublicRepo
      ? publicBranchesQuery.data?.branches
      : branchesData?.branches) as any) || []
  const isLoadingBranches = isPublicRepo
    ? publicBranchesQuery.isLoading
    : isLoadingConnectedBranches
  const refetchBranches = isPublicRepo
    ? publicBranchesQuery.refetch
    : refetchConnectedBranches

  // Repository metadata (description, language, pushed_at, default_branch, clone urls)
  const { data: repositoryData } = useQuery({
    queryKey: [
      'repository-search',
      project?.repo_owner,
      project?.repo_name,
      project?.git_provider_connection_id,
    ],
    queryFn: async () => {
      if (
        !project?.repo_owner ||
        !project?.repo_name ||
        !project?.git_provider_connection_id
      ) {
        return null
      }
      const response = await listRepositoriesByConnection({
        path: { connection_id: project.git_provider_connection_id },
        query: { search: project.repo_name, per_page: 100 },
        throwOnError: true,
      })
      return (
        response.data?.repositories?.find(
          (r: any) =>
            r.owner === project.repo_owner && r.name === project.repo_name
        ) || null
      )
    },
    enabled:
      !!project?.repo_owner &&
      !!project?.repo_name &&
      !!project?.git_provider_connection_id,
  })

  // Preset detection — authenticated repos
  const presetQuery = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repositoryData?.id || 0 },
    }),
    enabled: !!repositoryData?.id,
  })
  // Preset detection — public repos
  const publicPresetQuery = useQuery({
    ...detectPublicPresetsOptions({
      path: {
        provider: publicProvider,
        owner: project?.repo_owner || '',
        repo: project?.repo_name || '',
      },
      query: { base_url: publicRepository?.instanceUrl },
    }),
    enabled: isPublicRepo && !!project?.repo_owner && !!project?.repo_name,
  })
  const publicRepositoryQuery = useQuery({
    ...getPublicRepositoryOptions({
      path: {
        provider: publicProvider,
        owner: project?.repo_owner || '',
        repo: project?.repo_name || '',
      },
      query: { base_url: publicRepository?.instanceUrl },
    }),
    enabled: isPublicRepo && !!project?.repo_owner && !!project?.repo_name,
  })
  const repositoryDescription =
    repositoryData?.description ?? publicRepositoryQuery.data?.description
  const effectiveDefaultBranch =
    repositoryData?.default_branch ?? publicRepositoryQuery.data?.default_branch
  const publicPresetData = useMemo(() => {
    if (!publicPresetQuery.data?.presets?.length) return null
    return {
      presets: publicPresetQuery.data.presets.map((p: any) => ({
        preset: p.preset,
        presetLabel: p.preset_label,
        exposedPort: p.exposed_port,
        iconUrl: p.icon_url,
        projectType: p.project_type,
        path: p.path,
        composeFiles: p.compose_files,
      })),
    }
  }, [publicPresetQuery.data])
  const effectivePresetData = presetQuery.data || publicPresetData
  const effectivePresetLoading =
    presetQuery.isLoading ||
    publicPresetQuery.isLoading ||
    publicPresetQuery.isFetching

  // ---------------- Save helpers ----------------
  // The API expects a full update; build it from current project state plus overrides.
  const saveGitField = async (
    overrides: Partial<{
      main_branch: string
      preset: string
      directory: string
      preset_config: any
      repo_owner: string
      repo_name: string
      git_url: string
      is_public_repo: boolean
      git_provider_connection_id: number | null
    }>
  ) => {
    const presetCfg: any = (project?.preset_config as any) || {}
    const selectedPreset = overrides.preset ?? project.preset
    const selectedPresetConfig =
      overrides.preset === 'nixpacks' && overrides.preset_config === undefined
        ? presetConfigForSelection('nixpacks', presetCfg)
        : (overrides.preset_config ?? presetCfg ?? undefined)
    if (project.source_type === 'uploaded_source') {
      await updateProjectSettings.mutateAsync({
        body: {
          preset: selectedPreset,
          directory: overrides.directory ?? project.directory ?? './',
          preset_config: selectedPresetConfig,
        },
        path: { project_id: project.id },
      })
      await refetch()
      return
    }
    const body: Record<string, unknown> = {
      main_branch: overrides.main_branch ?? project.main_branch,
      preset: selectedPreset,
      directory: overrides.directory ?? project.directory ?? './',
      repo_owner: overrides.repo_owner ?? project.repo_owner!,
      repo_name: overrides.repo_name ?? project.repo_name!,
      preset_config: selectedPresetConfig,
    }
    if (isPublicRepo) {
      body.git_url =
        overrides.git_url ??
        project.git_url ??
        `https://github.com/${project.repo_owner}/${project.repo_name}`
      body.is_public_repo = true
      body.git_provider_connection_id = null
    } else {
      body.git_provider_connection_id =
        overrides.git_provider_connection_id ??
        project.git_provider_connection_id ??
        null
    }
    await updateGitSettings.mutateAsync({
      body: body as any,
      path: { project_id: project.id },
    })
    await refetch()
  }

  // ---------------- Inline editors ----------------
  const [editing, setEditing] = useState<
    | null
    | 'branch'
    | 'framework'
    | 'directory'
    | 'dockerfile'
    | 'buildContext'
    | 'composePath'
  >(null)
  const close = () => setEditing(null)

  // Tracks the branch-refresh button separately from `isLoadingBranches`
  // (React Query only flags that for the *first* fetch) so a manual refetch
  // spins the icon, and holds it for a minimum duration so a fast response
  // doesn't cut the spin off before it's visible.
  const [isRefreshingBranches, setIsRefreshingBranches] = useState(false)
  const handleRefreshBranches = async () => {
    setIsRefreshingBranches(true)
    try {
      await withMinDuration(() => refetchBranches())
    } finally {
      setIsRefreshingBranches(false)
    }
  }

  // Branch editor
  const [branchDraft, setBranchDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setBranchDraft(project.main_branch || '')
  }, [project.main_branch])

  // Directory editor
  const [directoryDraft, setDirectoryDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setDirectoryDraft(project.directory || './')
  }, [project.directory])

  // Dockerfile path editor
  const [dockerfileDraft, setDockerfileDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setDockerfileDraft(
      (project?.preset_config as any)?.dockerfilePath || 'Dockerfile'
    )
  }, [project?.preset_config])

  // Dockerfile build context editor (overrides the root directory as the
  // `docker build` context -- only needed when it must differ from where
  // the Dockerfile itself is discovered, e.g. a monorepo build that needs
  // sibling package sources outside the Dockerfile's own directory)
  const [buildContextDraft, setBuildContextDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setBuildContextDraft((project?.preset_config as any)?.buildContext || '')
  }, [project?.preset_config])

  // Compose path editor
  const [composePathDraft, setComposePathDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setComposePathDraft(
      (project?.preset_config as any)?.composePath || 'docker-compose.yml'
    )
  }, [project?.preset_config])

  const presetName = (project.preset || '').toString()
  const isDockerfilePreset = presetName === 'dockerfile'
  const isComposePreset =
    presetName === 'docker-compose' || presetName === 'dockercompose'

  // Compose override editor and redacted effective-deployment preview.
  const [overrideDraft, setOverrideDraft] = useState('')
  useEffect(() => {
    // The draft intentionally resets after a successful server refetch.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setOverrideDraft((project?.preset_config as any)?.composeOverride || '')
  }, [project?.preset_config])
  const [advancedComposeOpen, setAdvancedComposeOpen] = useState(false)
  const debouncedOverrideDraft = useDebounce(overrideDraft, 350)
  const composeConfig: any = (project.preset_config as any) || {}
  const savedComposeOverride = composeConfig.composeOverride || ''
  const composeOverrideDirty = overrideDraft !== savedComposeOverride
  const composePath = composeConfig.composePath || 'docker-compose.yml'
  const composeRepositoryPath = repositoryFilePath(
    project.directory,
    composePath
  )
  const excludedComposeServices: string[] =
    composeConfig.excludedServices || composeConfig.excluded_services || []
  const composePreviewQuery = useQuery({
    queryKey: [
      'effective-compose-preview',
      repositoryData?.id,
      publicProvider,
      publicRepository?.instanceUrl,
      project.repo_owner,
      project.repo_name,
      project.main_branch,
      composeRepositoryPath,
      debouncedOverrideDraft,
      excludedComposeServices,
    ],
    queryFn: ({ signal }) =>
      fetchComposePreview(
        isPublicRepo
          ? {
              kind: 'public',
              provider: publicProvider,
              owner: project.repo_owner || '',
              repo: project.repo_name || '',
              baseUrl: publicRepository?.instanceUrl,
            }
          : {
              kind: 'connected',
              repositoryId: repositoryData!.id,
            },
        {
          branch: project.main_branch,
          path: composeRepositoryPath,
          composeOverride: debouncedOverrideDraft || undefined,
          excludedServices: excludedComposeServices,
        },
        signal
      ),
    enabled:
      advancedComposeOpen &&
      isComposePreset &&
      (isPublicRepo
        ? !!project.repo_owner && !!project.repo_name
        : !!repositoryData?.id),
    retry: false,
  })
  const composePreviewRateLimited =
    isPublicRepo && isPublicRepositoryRateLimitError(composePreviewQuery.error)

  const navigate = useNavigate()
  const goToChangeRepo = () =>
    navigate(repositoryConnectionBasePath(project.slug))

  // GitLab webhook reinstall
  const reinstallWebhook = useMutation({
    ...reinstallGitlabWebhookMutation(),
    meta: { errorTitle: 'Failed to reinstall webhook' },
  })
  const isGitlab = currentProvider?.provider_type === 'gitlab'
  const hasWebhook = !!project.gitlab_webhook_id

  const handleReinstallWebhook = async () => {
    try {
      await reinstallWebhook.mutateAsync({ path: { project_id: project.id! } })
      await refetch()
      toast.success('Webhook reinstalled')
    } catch {
      toast.error(
        'Failed to reinstall webhook — check your GitLab user has Maintainer role on the repo.'
      )
    }
  }

  // Auto-deploy
  const handleAutoDeployToggle = async (enabled: boolean) => {
    try {
      await updateAutomaticDeploy.mutateAsync({
        path: { project_id: project.id! },
        body: { automatic_deploy: enabled },
      })
      await refetch()
      toast.success(enabled ? 'Auto-deploy enabled' : 'Auto-deploy disabled')
    } catch {
      toast.error('Failed to update')
    }
  }

  // Empty state — no repo connected.
  if (
    view === 'git' &&
    (!project.repo_owner || !project.repo_name) &&
    !isUploadedSource
  ) {
    return (
      <div className="space-y-6">
        <Card>
          <CardContent className="p-8 text-center space-y-4">
            <div className="mx-auto flex size-12 items-center justify-center rounded-full bg-muted">
              <GithubIcon className="size-6" />
            </div>
            <div className="space-y-1">
              <h3 className="text-base font-semibold">
                No repository connected
              </h3>
              <p className="text-sm text-muted-foreground">
                Connect a Git repository to enable deployments for this project.
              </p>
            </div>
            <Button onClick={goToChangeRepo}>
              <Plus className="size-4 mr-1" />
              Connect repository
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Build a link to the upstream repo. Prefer the stored git_url; fall back
  // to a provider-aware constructed URL so GitLab projects don't land on
  // github.com/<owner>/<repo> when git_url wasn't populated.
  const ghHref =
    project.git_url ||
    (currentProvider?.provider_type === 'gitlab'
      ? `https://gitlab.com/${project.repo_owner}/${project.repo_name}`
      : `https://github.com/${project.repo_owner}/${project.repo_name}`)

  // Helper for displaying short SHA
  const shortSha = (sha: string) => sha?.slice(0, 7)

  // Matches the backend's resolved default (`unwrap_or(false)` in
  // temps-entities/deployment_config.rs) -- GeneralSettings.tsx reads the
  // same field with the same default. This page previously defaulted to
  // `true`, which showed auto-deploy as ON for any project with the field
  // unset even though the backend treats it as OFF.
  const autoDeployOn = project.deployment_config?.automaticDeploy ?? false

  return (
    <div className="space-y-6">
      {!embedded && (
        <div className="space-y-1">
          <h2 className="text-xl font-semibold text-balance">
            {view === 'git' ? 'Git repository' : 'Build and deployment'}
          </h2>
          <p className="max-w-[72ch] text-pretty text-base/7 text-muted-foreground sm:text-sm/6">
            {view === 'git'
              ? 'Manage the source repository and the Git events that trigger deployments.'
              : 'Configure how Temps turns your source into a running application.'}
          </p>
        </div>
      )}

      {view === 'build' && !embedded && (
        <div className="flex flex-col gap-4 rounded-lg border p-4 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0 space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <div className="text-base font-medium sm:text-sm">
                Deployment source
              </div>
              <Badge variant="secondary">
                {isUploadedSource ? 'Uploaded archive' : 'Git repository'}
              </Badge>
            </div>
            <p className="max-w-[72ch] text-pretty text-base/7 text-muted-foreground sm:text-sm/6">
              {isUploadedSource
                ? 'Temps is deploying the source snapshot uploaded through Drop. It will not receive updates from Git.'
                : `Temps fetches ${project.repo_owner}/${project.repo_name} from Git for each deployment.`}
            </p>
          </div>
          <div className="flex shrink-0 flex-wrap gap-2">
            {isUploadedSource ? (
              <>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => navigate(`/projects/${project.slug}/drop`)}
                >
                  Upload new version
                </Button>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={goToChangeRepo}
                >
                  Connect repository
                </Button>
              </>
            ) : (
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => navigate(`/projects/${project.slug}/git`)}
              >
                View Git settings
              </Button>
            )}
          </div>
        </div>
      )}

      {/* ----------------- Repository card ----------------- */}
      {sections.showRepository && (
        <Card>
          <CardContent className="p-6 space-y-4">
            <div className="flex items-start gap-4">
              <div className="flex size-10 shrink-0 items-center justify-center rounded-md border bg-muted/40">
                {currentProvider?.provider_type === 'gitlab' ? (
                  <GitlabIcon className="size-5" />
                ) : (
                  <GithubIcon className="size-5" />
                )}
              </div>
              <div className="flex-1 min-w-0 space-y-1">
                <div className="flex flex-wrap items-center gap-2">
                  <a
                    href={ghHref}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="font-semibold text-base hover:underline truncate"
                  >
                    {project.repo_owner}/{project.repo_name}
                  </a>
                  <Badge variant="secondary" className="text-xs">
                    {isPublicRepo
                      ? 'Public'
                      : repositoryData?.private
                        ? 'Private'
                        : 'Connected'}
                  </Badge>
                </div>
                {repositoryDescription && (
                  <p className="text-sm text-muted-foreground line-clamp-2">
                    {repositoryDescription}
                  </p>
                )}
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                  {repositoryData?.pushed_at && (
                    <span>
                      Pushed <TimeAgo date={repositoryData.pushed_at} />
                    </span>
                  )}
                  {effectiveDefaultBranch && (
                    <span className="flex items-center gap-1">
                      <GitBranchIcon className="size-3" />
                      default{' '}
                      <span className="font-mono text-foreground">
                        {effectiveDefaultBranch}
                      </span>
                    </span>
                  )}
                  {currentConnection && (
                    <span>
                      via{' '}
                      <span className="text-foreground">
                        {currentConnection.account_name}
                      </span>{' '}
                      <span className="text-muted-foreground">
                        ({currentProvider?.name})
                      </span>
                    </span>
                  )}
                </div>
              </div>
              <div className="flex shrink-0 gap-2">
                <Button variant="outline" size="sm" onClick={goToChangeRepo}>
                  Change repository
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {sections.showUploadedSource && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">
              No Git repository connected
            </CardTitle>
            <CardDescription>
              This project currently deploys source archives uploaded through
              Drop. Connect a repository to enable deployments from commits and
              branches.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button size="sm" onClick={goToChangeRepo}>
              Connect repository
            </Button>
          </CardContent>
        </Card>
      )}

      {/* ----------------- Build configuration card ----------------- */}
      {(sections.showBuildConfiguration || sections.showGitAutomation) && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">
              {sections.showGitAutomation
                ? 'Production branch'
                : 'Build configuration'}
            </CardTitle>
            <CardDescription>
              {sections.showGitAutomation
                ? 'Commits pushed to this branch create production deployments.'
                : 'Framework, source directory, and runtime-specific build settings.'}
            </CardDescription>
          </CardHeader>
          <CardContent className="p-0">
            <ul className="divide-y">
              {/* Branch row */}
              {sections.showGitAutomation && (
                <InlineRow
                  label="Branch"
                  editing={editing === 'branch'}
                  onStartEdit={() => {
                    setBranchDraft(project.main_branch || '')
                    setEditing('branch')
                  }}
                  onCancel={close}
                  onSave={async () => {
                    if (!branchDraft || branchDraft === project.main_branch) {
                      close()
                      return
                    }
                    await saveGitField({ main_branch: branchDraft })
                    toast.success('Branch updated')
                    close()
                  }}
                  isPending={updateGitSettings.isPending}
                  display={
                    <div className="flex items-center gap-2 min-w-0">
                      <GitBranchIcon className="size-4 text-muted-foreground shrink-0" />
                      <span className="font-mono text-sm truncate">
                        {project.main_branch}
                      </span>
                      {effectiveDefaultBranch === project.main_branch && (
                        <Badge variant="outline" className="text-xs">
                          default
                        </Badge>
                      )}
                      {(() => {
                        const b = branches.find(
                          (br) => br.name === project.main_branch
                        )
                        if (b?.protected) {
                          return (
                            <Badge variant="outline" className="text-xs">
                              protected
                            </Badge>
                          )
                        }
                        return null
                      })()}
                      {(() => {
                        const b = branches.find(
                          (br) => br.name === project.main_branch
                        )
                        if (b?.commit_sha) {
                          return (
                            <span className="font-mono text-xs text-muted-foreground">
                              {shortSha(b.commit_sha)}
                            </span>
                          )
                        }
                        return null
                      })()}
                    </div>
                  }
                  editor={
                    <div className="flex flex-1 items-center gap-2">
                      {isLoadingBranches ? (
                        <div className="flex items-center gap-2 text-sm text-muted-foreground">
                          <Loader2 className="size-4 animate-spin" />
                          Loading branches…
                        </div>
                      ) : branches.length > 0 ? (
                        <Select
                          value={
                            branches.some((b) => b.name === branchDraft)
                              ? branchDraft
                              : '__custom__'
                          }
                          onValueChange={(v) => {
                            if (v === '__custom__') {
                              setBranchDraft('')
                            } else {
                              setBranchDraft(v)
                            }
                          }}
                        >
                          <SelectTrigger className="flex-1">
                            <SelectValue placeholder="Select a branch" />
                          </SelectTrigger>
                          <SelectContent>
                            {branches.map((b) => (
                              <SelectItem key={b.name} value={b.name}>
                                <div className="flex items-center gap-2">
                                  <GitBranchIcon className="size-4" />
                                  <span className="font-mono">{b.name}</span>
                                  {b.protected && (
                                    <Badge
                                      variant="outline"
                                      className="text-[10px] py-0"
                                    >
                                      protected
                                    </Badge>
                                  )}
                                  {b.name === effectiveDefaultBranch && (
                                    <Check className="size-3 text-green-500" />
                                  )}
                                  {b.commit_sha && (
                                    <span className="ml-auto font-mono text-xs text-muted-foreground">
                                      {shortSha(b.commit_sha)}
                                    </span>
                                  )}
                                </div>
                              </SelectItem>
                            ))}
                            <SelectItem value="__custom__">
                              <span className="text-muted-foreground">
                                Custom branch…
                              </span>
                            </SelectItem>
                          </SelectContent>
                        </Select>
                      ) : null}
                      {(branches.length === 0 ||
                        !branches.some((b) => b.name === branchDraft)) && (
                        <Input
                          value={branchDraft}
                          onChange={(e) => setBranchDraft(e.target.value)}
                          placeholder="Branch name"
                          className="flex-1 font-mono text-sm"
                          autoFocus
                        />
                      )}
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        onClick={handleRefreshBranches}
                        disabled={isLoadingBranches || isRefreshingBranches}
                        title="Refresh branches"
                      >
                        {isLoadingBranches || isRefreshingBranches ? (
                          <Loader2 className="size-3.5 animate-spin" />
                        ) : (
                          <RefreshCw className="size-3.5" />
                        )}
                      </Button>
                    </div>
                  }
                />
              )}

              {sections.showBuildConfiguration && (
                <>
                  {/* Framework row */}
                  <InlineRow
                    label="Framework"
                    editing={editing === 'framework'}
                    onStartEdit={() => setEditing('framework')}
                    onCancel={close}
                    hideSaveButtons
                    display={
                      <div className="flex items-center gap-2 min-w-0">
                        <FrameworkIcon
                          preset={project.preset as any}
                          className="size-5 shrink-0"
                        />
                        <span className="text-sm truncate">
                          {project.preset}
                        </span>
                      </div>
                    }
                    editor={
                      <div className="flex-1">
                        <FrameworkSelector
                          presetData={effectivePresetData as any}
                          isLoading={effectivePresetLoading}
                          error={presetQuery.error}
                          selectedPreset={(() => {
                            const dir = project.directory || './'
                            const norm =
                              dir === '.' || dir === './'
                                ? 'root'
                                : dir.startsWith('./')
                                  ? dir.slice(2)
                                  : dir
                            return `${project.preset}::${norm}`
                          })()}
                          onSelectPreset={async (value) => {
                            const { preset: slug, directory: dir } =
                              splitPresetSelection(value)
                            await saveGitField({ preset: slug, directory: dir })
                            toast.success('Framework updated')
                            close()
                          }}
                        />
                      </div>
                    }
                  />

                  {/* Directory row */}
                  <InlineRow
                    label="Root directory"
                    editing={editing === 'directory'}
                    onStartEdit={() => {
                      setDirectoryDraft(project.directory || './')
                      setEditing('directory')
                    }}
                    onCancel={close}
                    onSave={async () => {
                      const next = directoryDraft || './'
                      if (next === project.directory) {
                        close()
                        return
                      }
                      await saveGitField({ directory: next })
                      toast.success('Root directory updated')
                      close()
                    }}
                    isPending={updateGitSettings.isPending}
                    display={
                      <div className="flex items-center gap-2 min-w-0">
                        <FolderIcon className="size-4 text-muted-foreground shrink-0" />
                        <span className="font-mono text-sm truncate">
                          {project.directory || './'}
                        </span>
                      </div>
                    }
                    editor={
                      <Input
                        value={directoryDraft}
                        onChange={(e) => setDirectoryDraft(e.target.value)}
                        placeholder="./"
                        className="flex-1 font-mono text-sm"
                        autoFocus
                      />
                    }
                  />

                  {/* Dockerfile path — only when dockerfile preset */}
                  {isDockerfilePreset && (
                    <InlineRow
                      label="Dockerfile path"
                      editing={editing === 'dockerfile'}
                      onStartEdit={() => {
                        setDockerfileDraft(
                          (project?.preset_config as any)?.dockerfilePath ||
                            'Dockerfile'
                        )
                        setEditing('dockerfile')
                      }}
                      onCancel={close}
                      onSave={async () => {
                        const cfg = (project.preset_config as any) || {}
                        const next = dockerfileDraft || 'Dockerfile'
                        if (next === (cfg.dockerfilePath || 'Dockerfile')) {
                          close()
                          return
                        }
                        await saveGitField({
                          preset_config: {
                            ...cfg,
                            preset: 'dockerfile',
                            dockerfilePath: next,
                          },
                        })
                        toast.success('Dockerfile path updated')
                        close()
                      }}
                      isPending={updateGitSettings.isPending}
                      display={
                        <div className="flex items-center gap-2 min-w-0">
                          <FileIcon className="size-4 text-muted-foreground shrink-0" />
                          <span className="font-mono text-sm truncate">
                            {(project.preset_config as any)?.dockerfilePath ||
                              'Dockerfile'}
                          </span>
                        </div>
                      }
                      editor={
                        <Input
                          value={dockerfileDraft}
                          onChange={(e) => setDockerfileDraft(e.target.value)}
                          placeholder="Dockerfile"
                          className="flex-1 font-mono text-sm"
                          autoFocus
                        />
                      }
                    />
                  )}

                  {/* Build context — only when dockerfile preset. Overrides the
                docker build context (defaults to root directory above) for
                monorepos where the Dockerfile needs sibling package sources
                outside its own directory. */}
                  {isDockerfilePreset && (
                    <InlineRow
                      label="Build context"
                      editing={editing === 'buildContext'}
                      onStartEdit={() => {
                        setBuildContextDraft(
                          (project?.preset_config as any)?.buildContext || ''
                        )
                        setEditing('buildContext')
                      }}
                      onCancel={close}
                      onSave={async () => {
                        const cfg = (project.preset_config as any) || {}
                        const next = buildContextDraft.trim()
                        if (next === (cfg.buildContext || '')) {
                          close()
                          return
                        }
                        await saveGitField({
                          preset_config: {
                            ...cfg,
                            preset: 'dockerfile',
                            buildContext: next || undefined,
                          },
                        })
                        toast.success('Build context updated')
                        close()
                      }}
                      isPending={updateGitSettings.isPending}
                      display={
                        <div className="flex items-center gap-2 min-w-0">
                          <FolderIcon className="size-4 text-muted-foreground shrink-0" />
                          <span className="font-mono text-sm truncate">
                            {(project.preset_config as any)?.buildContext || (
                              <span className="text-muted-foreground italic">
                                same as root directory
                              </span>
                            )}
                          </span>
                        </div>
                      }
                      editor={
                        <Input
                          value={buildContextDraft}
                          onChange={(e) => setBuildContextDraft(e.target.value)}
                          placeholder={project.directory || './'}
                          className="flex-1 font-mono text-sm"
                          autoFocus
                        />
                      }
                    />
                  )}

                  {/* Compose path — only when compose preset */}
                  {isComposePreset && (
                    <InlineRow
                      label="Compose file"
                      editing={editing === 'composePath'}
                      onStartEdit={() => {
                        setComposePathDraft(
                          (project?.preset_config as any)?.composePath ||
                            'docker-compose.yml'
                        )
                        setEditing('composePath')
                      }}
                      onCancel={close}
                      onSave={async () => {
                        const cfg = (project.preset_config as any) || {}
                        const next = composePathDraft || 'docker-compose.yml'
                        if (
                          next === (cfg.composePath || 'docker-compose.yml')
                        ) {
                          close()
                          return
                        }
                        await saveGitField({
                          preset_config: {
                            ...cfg,
                            preset: 'docker-compose',
                            composePath: next,
                          },
                        })
                        toast.success('Compose path updated')
                        close()
                      }}
                      isPending={updateGitSettings.isPending}
                      display={
                        <div className="flex items-center gap-2 min-w-0">
                          <FileIcon className="size-4 text-muted-foreground shrink-0" />
                          <span className="font-mono text-sm truncate">
                            {(project.preset_config as any)?.composePath ||
                              'docker-compose.yml'}
                          </span>
                        </div>
                      }
                      editor={
                        <Input
                          value={composePathDraft}
                          onChange={(e) => setComposePathDraft(e.target.value)}
                          placeholder="docker-compose.yml"
                          className="flex-1 font-mono text-sm"
                          autoFocus
                        />
                      }
                    />
                  )}
                </>
              )}
            </ul>
          </CardContent>
        </Card>
      )}

      {/* ----------------- Compose routing and service controls ----------------- */}
      {sections.showBuildConfiguration && isComposePreset && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Docker Compose</CardTitle>
            <CardDescription>
              Choose what runs and which container ports receive public traffic.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <PublicPortsInline project={project} saveGitField={saveGitField} />

            <ExcludedServicesInline
              project={project}
              saveGitField={saveGitField}
              repositoryId={repositoryData?.id}
              isPublicRepo={isPublicRepo}
              isUploadedSource={isUploadedSource}
            />

            {!isUploadedSource && (
              <Collapsible
                open={advancedComposeOpen}
                onOpenChange={setAdvancedComposeOpen}
                className="group border-t pt-4"
              >
                <CollapsibleTrigger asChild>
                  <button
                    type="button"
                    className="flex w-full items-center justify-between rounded-md py-2 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    <div>
                      <div className="text-sm font-medium">
                        Advanced override
                      </div>
                      <p className="mt-0.5 text-xs text-muted-foreground">
                        Edit YAML and inspect the redacted Compose document
                        Temps will deploy.
                      </p>
                    </div>
                    <ChevronDown className="size-4 text-muted-foreground transition-transform group-data-[state=open]:rotate-180" />
                  </button>
                </CollapsibleTrigger>
                <CollapsibleContent className="space-y-3 pt-3">
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <Badge variant="outline" className="gap-1.5 font-normal">
                      <ShieldCheck className="size-3" />
                      {composePreviewQuery.data?.enabledServices.length ??
                        '—'}{' '}
                      enabled
                    </Badge>
                    {(composePreviewQuery.data?.disabledServices.length ?? 0) >
                      0 && (
                      <Badge variant="outline" className="font-normal">
                        {composePreviewQuery.data?.disabledServices.length}{' '}
                        disabled
                      </Badge>
                    )}
                    <Badge
                      variant="outline"
                      className="gap-1.5 border-emerald-500/30 bg-emerald-500/5 font-normal text-emerald-700 dark:text-emerald-300"
                    >
                      <EyeOff className="size-3" />
                      Environment values redacted
                    </Badge>
                  </div>

                  <div className="overflow-hidden rounded-lg border bg-background text-foreground lg:grid lg:grid-cols-2 lg:divide-x lg:divide-border dark:bg-zinc-950 dark:text-zinc-100 dark:lg:divide-white/10">
                    <section className="min-w-0">
                      <div className="flex min-h-14 items-center justify-between gap-3 border-b bg-muted/30 px-3 py-2 dark:border-white/10 dark:bg-zinc-900">
                        <div className="min-w-0">
                          <div className="text-sm font-medium text-foreground dark:text-zinc-100">
                            Override
                          </div>
                          <div className="truncate text-sm/5 text-muted-foreground dark:text-zinc-400">
                            docker-compose.temps-override.yml
                          </div>
                        </div>
                        <div className="flex items-center gap-1.5">
                          {composeOverrideDirty && (
                            <div className="text-sm text-amber-600 dark:text-amber-400">
                              Unsaved
                            </div>
                          )}
                          <Button
                            type="button"
                            variant="ghost"
                            size="sm"
                            className="h-8 px-2 text-sm dark:text-zinc-300 dark:hover:bg-white/10 dark:hover:text-white dark:disabled:text-zinc-600"
                            disabled={!composeOverrideDirty}
                            onClick={() =>
                              setOverrideDraft(savedComposeOverride)
                            }
                          >
                            Reset
                          </Button>
                          <Button
                            type="button"
                            size="sm"
                            className="h-8 px-3 text-sm dark:bg-zinc-100 dark:text-zinc-950 dark:hover:bg-white dark:hover:text-zinc-950 dark:disabled:bg-zinc-800 dark:disabled:text-zinc-500"
                            disabled={
                              !composeOverrideDirty ||
                              updateGitSettings.isPending ||
                              composePreviewQuery.isFetching ||
                              composePreviewQuery.isError
                            }
                            onClick={async () => {
                              await saveGitField({
                                preset_config: composeSettingPatch(
                                  composeConfig,
                                  'composeOverride',
                                  overrideDraft || null
                                ),
                              })
                              toast.success('Compose override saved')
                            }}
                          >
                            {updateGitSettings.isPending && (
                              <Loader2 className="size-4 animate-spin" />
                            )}
                            Save
                          </Button>
                        </div>
                      </div>
                      <Editor
                        height="360px"
                        defaultLanguage="yaml"
                        value={overrideDraft}
                        onChange={(value) => setOverrideDraft(value || '')}
                        theme={composeEditorTheme}
                        options={{
                          minimap: { enabled: false },
                          automaticLayout: true,
                          fontSize: 12,
                          lineHeight: 19,
                          tabSize: 2,
                          insertSpaces: true,
                          scrollBeyondLastLine: false,
                          wordWrap: 'on',
                          padding: { top: 12, bottom: 12 },
                          renderLineHighlight: 'line',
                          overviewRulerBorder: false,
                          hideCursorInOverviewRuler: true,
                        }}
                      />
                    </section>

                    <section className="min-w-0 border-t lg:border-t-0 dark:border-white/10">
                      <div className="flex min-h-14 items-center justify-between gap-3 border-b bg-muted/30 px-3 py-2 dark:border-white/10 dark:bg-zinc-900">
                        <div className="min-w-0">
                          <div className="text-sm font-medium text-foreground dark:text-zinc-100">
                            Effective deployment
                          </div>
                          <div className="truncate text-sm/5 text-muted-foreground dark:text-zinc-400">
                            Repository + enabled services + override
                          </div>
                        </div>
                        {composePreviewQuery.isFetching ? (
                          <div className="flex shrink-0 items-center gap-1.5 text-sm text-muted-foreground dark:text-zinc-400">
                            <Loader2 className="size-4 animate-spin" /> Updating
                          </div>
                        ) : composePreviewQuery.data ? (
                          <div className="flex shrink-0 items-center gap-1.5 text-sm text-emerald-700 dark:text-emerald-400">
                            <Check className="size-4" /> Current
                          </div>
                        ) : null}
                      </div>
                      {composePreviewQuery.isError ? (
                        <div className="flex h-[360px] items-center justify-center p-6">
                          <div
                            className={cn(
                              'max-w-md rounded-md border p-4 text-center',
                              composePreviewRateLimited
                                ? 'border-amber-500/30 bg-amber-500/5 dark:border-amber-400/30 dark:bg-amber-950/20'
                                : 'border-destructive/30 bg-destructive/5 dark:border-red-400/30 dark:bg-red-950/30'
                            )}
                          >
                            <p
                              className={cn(
                                'text-sm font-medium',
                                composePreviewRateLimited
                                  ? 'text-amber-800 dark:text-amber-300'
                                  : 'text-destructive dark:text-red-300'
                              )}
                            >
                              {composePreviewRateLimited
                                ? `${publicProvider === 'github' ? 'GitHub' : 'GitLab'} limit reached`
                                : 'Preview unavailable'}
                            </p>
                            <p className="mt-1 text-sm/6 text-muted-foreground dark:text-zinc-400">
                              {composePreviewRateLimited
                                ? publicProvider === 'github'
                                  ? 'GitHub’s API limit was reached. When you are signed in, Temps automatically uses your valid GitHub connection. Check or reconnect it below, then retry. You can also install a GitHub App without creating or pasting a personal token.'
                                  : 'Anonymous GitLab requests are temporarily exhausted. Connect your own GitLab account, then return here and use Change repository to attach it.'
                                : composePreviewErrorMessage(
                                    composePreviewQuery.error
                                  )}
                            </p>
                            {composePreviewRateLimited && (
                              <div className="mt-4 flex flex-wrap justify-center gap-2">
                                <Button
                                  size="sm"
                                  onClick={() => navigate('/git-providers')}
                                >
                                  {publicProvider === 'github' ? (
                                    <GithubIcon className="size-4" />
                                  ) : (
                                    <GitlabIcon className="size-4" />
                                  )}
                                  Connect{' '}
                                  {publicProvider === 'github'
                                    ? 'GitHub'
                                    : 'GitLab'}
                                </Button>
                                <Button
                                  size="sm"
                                  variant="outline"
                                  disabled={composePreviewQuery.isFetching}
                                  onClick={() => composePreviewQuery.refetch()}
                                >
                                  <RefreshCw className="size-4" />
                                  Retry
                                </Button>
                              </div>
                            )}
                          </div>
                        </div>
                      ) : composePreviewQuery.data ? (
                        <Editor
                          height="360px"
                          defaultLanguage="yaml"
                          value={composePreviewQuery.data.effectiveCompose}
                          theme={composeEditorTheme}
                          options={{
                            readOnly: true,
                            domReadOnly: true,
                            minimap: { enabled: false },
                            automaticLayout: true,
                            fontSize: 12,
                            lineHeight: 19,
                            scrollBeyondLastLine: false,
                            wordWrap: 'on',
                            padding: { top: 12, bottom: 12 },
                            renderLineHighlight: 'none',
                            overviewRulerBorder: false,
                            hideCursorInOverviewRuler: true,
                          }}
                        />
                      ) : (
                        <div className="flex h-[360px] items-center justify-center gap-2 text-sm text-muted-foreground">
                          <Loader2 className="size-4 animate-spin" />
                          Loading repository Compose file…
                        </div>
                      )}
                    </section>
                  </div>

                  <p className="text-[11px] leading-relaxed text-muted-foreground">
                    This preview applies disabled services and your override.
                    Temps-managed security, network, labels, and runtime
                    environment layers are added during deployment and cannot be
                    edited here. Environment and build-argument values are
                    always replaced with{' '}
                    <span className="font-mono text-foreground">
                      &lt;redacted&gt;
                    </span>
                    .
                  </p>
                </CollapsibleContent>
              </Collapsible>
            )}
          </CardContent>
        </Card>
      )}

      {/* ----------------- Deployment behavior ----------------- */}
      {sections.showGitAutomation && (
        <Card>
          <CardContent className="p-0 divide-y">
            <div className="flex items-center gap-4 p-6">
              <Switch
                checked={autoDeployOn}
                onCheckedChange={handleAutoDeployToggle}
              />
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium">Automatic deployments</div>
                <p className="text-xs text-muted-foreground">
                  {autoDeployOn ? (
                    <>
                      Pushes to{' '}
                      <span className="font-mono text-foreground">
                        {project.main_branch}
                      </span>{' '}
                      trigger a new deployment.
                    </>
                  ) : (
                    <>
                      Pushes to{' '}
                      <span className="font-mono text-foreground">
                        {project.main_branch}
                      </span>{' '}
                      will not deploy. Use manual deploy or the API.
                    </>
                  )}
                </p>
              </div>
            </div>

            {isGitlab && (
              <div className="flex items-center gap-4 p-6">
                <div
                  className={cn(
                    'flex size-9 shrink-0 items-center justify-center rounded-full',
                    hasWebhook ? 'bg-green-500/10' : 'bg-amber-500/10'
                  )}
                >
                  {hasWebhook ? (
                    <Check className="size-4 text-green-600 dark:text-green-400" />
                  ) : (
                    <RefreshCw className="size-4 text-amber-600 dark:text-amber-400" />
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <div className="text-sm font-medium">
                    {hasWebhook
                      ? 'GitLab webhook installed'
                      : 'GitLab webhook not installed'}
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {hasWebhook ? (
                      <>
                        Push events from{' '}
                        <span className="font-mono text-foreground">
                          {project.repo_owner}/{project.repo_name}
                        </span>{' '}
                        reach Temps automatically.
                      </>
                    ) : (
                      <>
                        We couldn’t install the webhook automatically. Your
                        GitLab user needs the Maintainer role on{' '}
                        <span className="font-mono text-foreground">
                          {project.repo_owner}/{project.repo_name}
                        </span>
                        .
                      </>
                    )}
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleReinstallWebhook}
                  disabled={reinstallWebhook.isPending}
                >
                  {reinstallWebhook.isPending && (
                    <Loader2 className="size-3 mr-1 animate-spin" />
                  )}
                  {hasWebhook ? 'Reinstall' : 'Install webhook'}
                </Button>
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}

// One row in the build configuration list. Click anywhere on the row to edit;
// while editing, the editor occupies the row, and Save/Cancel sit on the right.
function InlineRow({
  label,
  display,
  editor,
  editing,
  onStartEdit,
  onSave,
  onCancel,
  isPending,
  hideSaveButtons,
}: {
  label: string
  display: React.ReactNode
  editor: React.ReactNode
  editing: boolean
  onStartEdit: () => void
  onSave?: () => void | Promise<void>
  onCancel: () => void
  isPending?: boolean
  hideSaveButtons?: boolean
}) {
  return (
    <li className="px-6 py-4">
      <div className="flex items-center gap-4">
        <div className="w-32 shrink-0 text-sm text-muted-foreground">
          {label}
        </div>
        <div className="flex-1 min-w-0">
          {editing ? (
            <div
              className="flex items-center gap-2"
              onKeyDown={(e) => {
                // Allow Enter to save, Esc to cancel — but only on simple inputs
                // (let textarea handle Enter natively).
                const target = e.target as HTMLElement
                const isTextarea = target.tagName === 'TEXTAREA'
                if (e.key === 'Enter' && !isTextarea && !e.shiftKey) {
                  e.preventDefault()
                  if (!hideSaveButtons && onSave && !isPending) onSave()
                } else if (e.key === 'Escape') {
                  e.preventDefault()
                  onCancel()
                }
              }}
            >
              <div className="flex-1 min-w-0">{editor}</div>
              {!hideSaveButtons && (
                <>
                  <Button variant="ghost" size="sm" onClick={onCancel}>
                    Cancel
                  </Button>
                  <Button size="sm" onClick={onSave} disabled={isPending}>
                    {isPending && (
                      <Loader2 className="size-3 mr-1 animate-spin" />
                    )}
                    Save
                  </Button>
                </>
              )}
              {hideSaveButtons && (
                <Button variant="ghost" size="sm" onClick={onCancel}>
                  Cancel
                </Button>
              )}
            </div>
          ) : (
            <button
              type="button"
              onClick={onStartEdit}
              className="group flex w-full items-center justify-between gap-2 rounded-md py-1 px-2 -mx-2 text-left hover:bg-accent/40 focus-visible:bg-accent/40 focus-visible:outline-none"
            >
              <div className="flex-1 min-w-0">{display}</div>
              <span className="text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100">
                Edit
              </span>
            </button>
          )}
        </div>
      </div>
    </li>
  )
}

function PublicPortsInline({
  project,
  saveGitField,
}: {
  project: ProjectResponse
  saveGitField: (overrides: any) => Promise<void>
}) {
  type PublicRoute = {
    service: string
    port: number
    published?: number
  }
  const cfg: any = (project.preset_config as any) || {}
  const ports: PublicRoute[] = cfg.publicPorts || cfg.public_ports || []
  const [draft, setDraft] = useState<PublicRoute[]>(ports)
  const [dirty, setDirty] = useState(false)
  const [saving, setSaving] = useState(false)
  const persistedServices: ComposeServicePorts[] = (
    cfg.composeServices ||
    cfg.compose_services ||
    []
  ).map((service: any) => ({
    name: service.name,
    image: service.image,
    ports: (service.ports || []).map((port: any) => ({
      target: Number(port.target),
      published:
        port.published === undefined ? undefined : Number(port.published),
      protocol: port.protocol || 'tcp',
    })),
  }))
  const effectiveServices = mergeComposeServicePorts(
    persistedServices,
    parseComposeOverridePorts(cfg.composeOverride || cfg.compose_override)
  )
  const serviceNames = Array.from(
    new Set([
      ...(cfg.composeServices || cfg.compose_services || []).map(
        (service: any) => service.name as string
      ),
      ...effectiveServices.map((service) => service.name),
    ])
  ).sort()
  useEffect(() => {
    // Public routes are editable drafts that reset when the saved config changes.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setDraft(cfg.publicPorts || cfg.public_ports || [])
    setDirty(false)
  }, [project.preset_config])

  const update = (next: PublicRoute[]) => {
    setDraft(next)
    setDirty(true)
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <Route className="size-4 text-muted-foreground" />
            <Label className="text-sm font-medium">Public routes</Label>
          </div>
          <p className="text-pretty text-base/7 text-muted-foreground sm:text-sm/6">
            Choose the Compose service and port mapping for each public URL.
            Temps uses the published host port when running on the host and the
            container port when running in Docker.
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => {
              const firstService = effectiveServices.find(
                (service) => service.ports.length > 0
              )
              update([
                ...draft,
                {
                  service: firstService?.name || serviceNames[0] || '',
                  port: firstService?.ports[0]?.target || 0,
                  published: firstService?.ports[0]?.published,
                },
              ])
            }}
          >
            <Plus className="mr-1 size-4" />
            Add
          </Button>
        </div>
      </div>

      {draft.length === 0 ? (
        <div className="rounded-md border border-dashed px-4 py-5 text-center">
          <p className="text-base font-medium sm:text-sm">No public routes</p>
          <p className="text-base/7 text-muted-foreground sm:text-sm/6">
            Services remain private until you expose a container port.
          </p>
        </div>
      ) : (
        <div className="space-y-2">
          {draft.map((row, i) => {
            const service = effectiveServices.find(
              (candidate) => candidate.name === row.service
            )
            const selected = findComposePortMapping(
              effectiveServices,
              row.service,
              row.port
            )
            const selectablePorts = Array.from(
              new Map(
                (service?.ports || []).map((port) => [port.target, port])
              ).values()
            )
            return (
              <div key={i} className="rounded-md border bg-muted/20 p-3">
                <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_180px_auto] sm:items-end">
                  <div className="space-y-1.5">
                    <div className="flex items-center gap-2">
                      <Label
                        htmlFor={`compose-service-${i}`}
                        className="text-sm text-muted-foreground"
                      >
                        Service
                      </Label>
                      {i === 0 && (
                        <Badge
                          variant="outline"
                          className="h-5 text-[10px] font-normal"
                        >
                          Primary URL
                        </Badge>
                      )}
                    </div>
                    <ComposeServiceCombobox
                      id={`compose-service-${i}`}
                      value={row.service}
                      services={effectiveServices}
                      onValueChange={(nextService) => {
                        const next = [...draft]
                        const suggestedMapping = effectiveServices.find(
                          (candidate) => candidate.name === nextService
                        )?.ports[0]
                        next[i] = {
                          service: nextService,
                          port: suggestedMapping?.target ?? row.port,
                          published: suggestedMapping?.published,
                        }
                        update(next)
                      }}
                    />
                  </div>
                  <div className="space-y-1.5">
                    <Label
                      htmlFor={`compose-port-${i}`}
                      className="text-sm text-muted-foreground"
                    >
                      Port mapping
                    </Label>
                    {service && selectablePorts.length > 0 ? (
                      <Select
                        value={String(selected?.target || row.port || '')}
                        onValueChange={(value) => {
                          const next = [...draft]
                          const mapping = selectablePorts.find(
                            (port) => port.target === Number(value)
                          )
                          next[i] = {
                            ...next[i],
                            port: Number(value),
                            published: mapping?.published,
                          }
                          update(next)
                        }}
                      >
                        <SelectTrigger
                          id={`compose-port-${i}`}
                          className="h-11 font-mono text-base sm:h-9 sm:text-sm"
                          aria-label={`Port mapping for ${row.service}`}
                        >
                          <SelectValue placeholder="Select a port" />
                        </SelectTrigger>
                        <SelectContent>
                          {!selected && row.port > 0 && (
                            <SelectItem value={String(row.port)}>
                              <span className="font-mono tabular-nums">
                                {row.port} · Custom port
                              </span>
                            </SelectItem>
                          )}
                          {selectablePorts.map((port) => (
                            <SelectItem
                              key={`${port.target}/${port.protocol}`}
                              value={String(port.target)}
                            >
                              <div className="flex min-w-0 items-center gap-2">
                                <div className="font-mono tabular-nums">
                                  {port.published
                                    ? `${port.published} → ${port.target}/${port.protocol}`
                                    : `${port.target}/${port.protocol}`}
                                </div>
                              </div>
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    ) : (
                      <Input
                        id={`compose-port-${i}`}
                        name={`compose-port-${i}`}
                        aria-label={`Service port for ${row.service || `route ${i + 1}`}`}
                        type="number"
                        min={1}
                        max={65535}
                        value={row.port || ''}
                        placeholder="Enter a port"
                        className="h-11 font-mono text-base sm:h-9 sm:text-sm"
                        onChange={(e) => {
                          const next = [...draft]
                          next[i] = {
                            ...next[i],
                            port: Number(e.target.value),
                          }
                          update(next)
                        }}
                      />
                    )}
                  </div>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-11 shrink-0 sm:size-9"
                    aria-label={`Remove route ${i + 1}`}
                    onClick={() => update(draft.filter((_, j) => j !== i))}
                  >
                    <Trash2 className="size-4 stroke-muted-foreground" />
                  </Button>
                </div>
                {selected?.published ? (
                  <p className="mt-2 text-base/7 text-muted-foreground sm:text-sm/6">
                    Compose publishes host{' '}
                    <span className="font-mono text-foreground">
                      {selected.published}
                    </span>{' '}
                    → container{' '}
                    <span className="font-mono text-foreground">
                      {selected.target}/{selected.protocol}
                    </span>
                    . Temps automatically uses the reachable side.
                  </p>
                ) : service && service.ports.length > 0 ? (
                  <p className="mt-2 text-base/7 text-muted-foreground sm:text-sm/6">
                    Suggested from the effective Compose configuration.
                  </p>
                ) : (
                  <p className="mt-2 text-base/7 text-muted-foreground sm:text-sm/6">
                    No declared port found for this service. You can still enter
                    a known container port manually.
                  </p>
                )}
              </div>
            )
          })}
        </div>
      )}

      {dirty && (
        <div className="flex items-center justify-end gap-2 pt-2 border-t">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => {
              setDraft(cfg.publicPorts || cfg.public_ports || [])
              setDirty(false)
            }}
          >
            Reset
          </Button>
          <Button
            type="button"
            size="sm"
            disabled={saving}
            onClick={async () => {
              setSaving(true)
              try {
                const filtered = draft
                  .filter((p) => p.service && p.port > 0)
                  .map((route) => {
                    const mapping = findComposePortMapping(
                      effectiveServices,
                      route.service,
                      route.port
                    )
                    return {
                      service: route.service,
                      port: mapping?.target ?? route.port,
                      published: mapping?.published ?? route.published,
                    }
                  })
                await saveGitField({
                  preset_config: composeSettingPatch(
                    cfg,
                    'publicPorts',
                    filtered
                  ),
                })
                toast.success('Public routes saved')
                setDirty(false)
              } finally {
                setSaving(false)
              }
            }}
          >
            {saving && <Loader2 className="size-3 mr-1 animate-spin" />}
            Save routes
          </Button>
        </div>
      )}
    </div>
  )
}

function ExcludedServicesInline({
  project,
  saveGitField,
  repositoryId,
  isPublicRepo,
  isUploadedSource,
}: {
  project: ProjectResponse
  saveGitField: (overrides: any) => Promise<void>
  /** Real repository ID for connected repos (undefined for public "git URL" imports, which use `isPublicRepo` + `project.repo_owner`/`repo_name` instead). */
  repositoryId: number | undefined
  isPublicRepo: boolean
  isUploadedSource: boolean
}) {
  const cfg: any = (project.preset_config as any) || {}
  const composePath = cfg.composePath || 'docker-compose.yml'
  const composeRepositoryPath = repositoryFilePath(
    project.directory,
    composePath
  )
  const excluded: string[] = cfg.excludedServices || cfg.excluded_services || []
  const relaxedCapabilities: string[] =
    cfg.relaxedCapabilityServices || cfg.relaxed_capability_services || []
  const unsandboxedServices: string[] =
    cfg.unsandboxedServices || cfg.unsandboxed_services || []
  const [saving, setSaving] = useState(false)
  const [pendingUnsandboxService, setPendingUnsandboxService] = useState<
    string | null
  >(null)
  const publicProvider = publicRepositoryProvider(project.git_url)
  const publicRepository = parsePublicRepositoryUrl(project.git_url)

  // Persisted snapshot (captured at creation, refreshed after every
  // successful deploy) is the primary data source — no live git fetch on
  // every page load. This keeps the checklist usable even if the repo is
  // temporarily unreachable or access was revoked.
  const persistedServices: {
    name: string
    image?: string
    looksLikeDatabase: boolean
    detectedServiceType?: string | null
    ports: ComposeServicePorts['ports']
  }[] = (cfg.composeServices || cfg.compose_services || []).map((s: any) => ({
    name: s.name,
    image: s.image,
    looksLikeDatabase: s.looksLikeDatabase ?? s.looks_like_database ?? false,
    detectedServiceType:
      s.detectedServiceType ?? s.detected_service_type ?? undefined,
    ports: s.ports || [],
  }))

  // On-demand only: re-parses the compose file from the repo when the user
  // explicitly clicks "Sync from repository" (e.g. to preview a service
  // added upstream before the next deploy). Same connected/public split used
  // elsewhere in this file and in the New Project wizard.
  const { isFetching: isSyncingConnected, refetch: refetchConnected } =
    useQuery({
      ...getRepositoryComposeServicesLiveOptions({
        path: { repository_id: repositoryId || 0 },
        query: { branch: project.main_branch, path: composeRepositoryPath },
      }),
      enabled: false,
    })
  const { isFetching: isSyncingPublic, refetch: refetchPublic } = useQuery({
    ...getPublicComposeServicesOptions({
      path: {
        provider: publicProvider,
        owner: project.repo_owner || '',
        repo: project.repo_name || '',
      },
      query: {
        branch: project.main_branch,
        path: composeRepositoryPath,
        base_url: publicRepository?.instanceUrl,
      },
    }),
    enabled: false,
  })
  const isSyncing = isSyncingConnected || isSyncingPublic

  const sync = async () => {
    const result = isPublicRepo
      ? await refetchPublic()
      : await refetchConnected()
    const raw = result.data?.services
    if (!raw) {
      toast.error(`Couldn't read services from ${composeRepositoryPath}`)
      return
    }
    const refreshed = raw.map((s) => ({
      name: s.name,
      image: s.image,
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
    const refreshedNames = new Set(refreshed.map((s) => s.name))
    const filteredExcluded = excluded.filter((name) => refreshedNames.has(name))
    const filteredRelaxed = relaxedCapabilities.filter((name) =>
      refreshedNames.has(name)
    )
    const filteredUnsandboxed = unsandboxedServices.filter((name) =>
      refreshedNames.has(name)
    )
    setSaving(true)
    try {
      await saveGitField({
        preset_config: {
          ...cfg,
          preset: 'docker-compose',
          composeServices: refreshed,
          excludedServices: filteredExcluded,
          relaxedCapabilityServices: filteredRelaxed,
          unsandboxedServices: filteredUnsandboxed,
        },
      })
      toast.success(`Synced ${refreshed.length} service(s) from ${composePath}`)
    } finally {
      setSaving(false)
    }
  }

  const services = persistedServices

  const toggle = async (serviceName: string, included: boolean) => {
    const next = included
      ? excluded.filter((s) => s !== serviceName)
      : [...excluded, serviceName]
    setSaving(true)
    try {
      await saveGitField({
        preset_config: composeSettingPatch(cfg, 'excludedServices', next),
      })
      toast.success(
        included
          ? `${serviceName} will be deployed`
          : `${serviceName} excluded from deployment`
      )
    } finally {
      setSaving(false)
    }
  }

  const toggleCapabilities = async (serviceName: string, relaxed: boolean) => {
    const next = relaxed
      ? [...relaxedCapabilities, serviceName]
      : relaxedCapabilities.filter((s) => s !== serviceName)
    setSaving(true)
    try {
      await saveGitField({
        preset_config: composeSettingPatch(
          cfg,
          'relaxedCapabilityServices',
          next
        ),
      })
      toast.success(
        relaxed
          ? `${serviceName} granted elevated permissions`
          : `${serviceName} back to strict permissions`
      )
    } finally {
      setSaving(false)
    }
  }

  const toggleSandbox = async (serviceName: string, disabled: boolean) => {
    const next = withComposeSandboxDisabled(
      relaxedCapabilities,
      unsandboxedServices,
      serviceName,
      disabled
    )
    setSaving(true)
    try {
      await saveGitField({
        preset_config: {
          ...cfg,
          preset: 'docker-compose',
          unsandboxedServices: next.unsandboxedServices,
          relaxedCapabilityServices: next.relaxedCapabilityServices,
        },
      })
      toast.success(
        disabled
          ? `${serviceName} will deploy without the Temps sandbox`
          : `${serviceName} will use the Temps sandbox`
      )
    } finally {
      setSaving(false)
      setPendingUnsandboxService(null)
    }
  }

  if (!repositoryId && !isPublicRepo && !isUploadedSource) {
    return null
  }

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-2">
        <div>
          <Label className="text-sm font-medium">Compose services</Label>
          <p className="text-xs text-muted-foreground mt-0.5">
            Uncheck a service to skip deploying it entirely — e.g. a raw
            database container, which won’t have Temps backup/restore. Services
            run with all Linux container permissions dropped by default; some
            official images (databases like postgres/mysql, but also others such
            as Gitea) need a few back to fix ownership on their data volume at
            startup — if a service fails with “Operation not permitted” errors,
            enable “Elevated permissions” for it below. For images that remain
            incompatible, “Disable sandbox” restores Docker’s normal runtime
            permissions for only that service.
          </p>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 shrink-0 text-xs"
          disabled={isUploadedSource || isSyncing || saving}
          onClick={sync}
          title={
            isUploadedSource
              ? 'Upload a new source archive to refresh Compose services'
              : undefined
          }
        >
          <RefreshCw className={cn('h-3 w-3', isSyncing && 'animate-spin')} />
          {isUploadedSource
            ? 'Refresh with new upload'
            : 'Sync from repository'}
        </Button>
      </div>

      {services.length === 0 ? (
        <p className="text-xs text-muted-foreground italic py-2">
          No services detected yet — captured automatically after your next
          deploy, or click “Sync from repository” to check {composePath} now.
        </p>
      ) : (
        <TooltipProvider>
          <div className="space-y-1.5">
            {services.map((service) => {
              const included = !excluded.includes(service.name)
              const hasElevatedPermissions = relaxedCapabilities.includes(
                service.name
              )
              const isUnsandboxed = unsandboxedServices.includes(service.name)
              return (
                <div
                  key={service.name}
                  className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-muted/50"
                >
                  <Checkbox
                    checked={included}
                    disabled={saving}
                    onCheckedChange={(checked) =>
                      toggle(service.name, checked === true)
                    }
                    id={`excluded-service-${service.name}`}
                  />
                  <label
                    htmlFor={`excluded-service-${service.name}`}
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
                        This looks like a database container — it won’t have
                        Temps backup/restore, and it may need elevated
                        permissions to start (see the toggle to the right).
                        Consider excluding it and using a Temps-managed database
                        instead.
                      </TooltipContent>
                    </Tooltip>
                  )}
                  <div className="flex items-center gap-1.5 pl-2 border-l">
                    <Checkbox
                      checked={hasElevatedPermissions}
                      disabled={saving || !included || isUnsandboxed}
                      onCheckedChange={(checked) =>
                        toggleCapabilities(service.name, checked === true)
                      }
                      id={`relaxed-capabilities-${service.name}`}
                    />
                    <label
                      htmlFor={`relaxed-capabilities-${service.name}`}
                      className={cn(
                        'text-xs cursor-pointer whitespace-nowrap',
                        included
                          ? 'text-muted-foreground'
                          : 'text-muted-foreground/50'
                      )}
                    >
                      Elevated permissions
                    </label>
                  </div>
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <div className="flex items-center gap-1.5 pl-2 border-l">
                        <Checkbox
                          checked={isUnsandboxed}
                          disabled={saving || !included}
                          onCheckedChange={(checked) => {
                            if (checked === true) {
                              setPendingUnsandboxService(service.name)
                            } else {
                              void toggleSandbox(service.name, false)
                            }
                          }}
                          id={`unsandboxed-service-${service.name}`}
                        />
                        <label
                          htmlFor={`unsandboxed-service-${service.name}`}
                          className={cn(
                            'text-xs cursor-pointer whitespace-nowrap flex items-center gap-1',
                            isUnsandboxed
                              ? 'text-destructive'
                              : included
                                ? 'text-muted-foreground'
                                : 'text-muted-foreground/50'
                          )}
                        >
                          <ShieldOff className="h-3 w-3" />
                          Disable sandbox
                        </label>
                      </div>
                    </TooltipTrigger>
                    <TooltipContent className="max-w-xs">
                      Removes Temps’ capability drop, privilege-escalation
                      guard, PID limit, and Docker init wrapper for this
                      service. Use only for a trusted image that cannot run with
                      elevated permissions.
                    </TooltipContent>
                  </Tooltip>
                </div>
              )
            })}
          </div>
        </TooltipProvider>
      )}

      <AlertDialog
        open={pendingUnsandboxService !== null}
        onOpenChange={(open) => !open && setPendingUnsandboxService(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-destructive" />
              Disable the Temps sandbox?
            </AlertDialogTitle>
            <AlertDialogDescription>
              {pendingUnsandboxService} will run with Docker’s normal runtime
              permissions. Temps will no longer drop Linux capabilities, prevent
              privilege escalation, enforce its PID limit, or place Docker init
              ahead of the image entrypoint for this service. Only continue if
              you trust the image and elevated permissions were not enough.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Keep sandbox enabled</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              onClick={() => {
                if (pendingUnsandboxService) {
                  void toggleSandbox(pendingUnsandboxService, true)
                }
              }}
            >
              Disable sandbox
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

function ComposeServiceCombobox({
  id,
  value,
  services,
  onValueChange,
}: {
  id: string
  value: string
  services: ComposeServicePorts[]
  onValueChange: (value: string) => void
}) {
  const [open, setOpen] = useState(false)
  const selected = services.find((service) => service.name === value)

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          id={id}
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          aria-label="Select Compose service"
          className="h-11 w-full justify-between gap-3 px-3 font-normal sm:h-9"
        >
          {selected ? (
            <div className="flex min-w-0 items-center gap-2 text-base sm:text-sm">
              <div className="shrink-0 font-mono">{selected.name}</div>
              {selected.image && (
                <div className="truncate text-muted-foreground">
                  {selected.image}
                </div>
              )}
            </div>
          ) : (
            <div className="text-base text-muted-foreground sm:text-sm">
              Select a Compose service
            </div>
          )}
          <ChevronsUpDown className="size-4 shrink-0 stroke-muted-foreground" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-[var(--radix-popover-trigger-width)] p-0"
      >
        <Command>
          <CommandInput placeholder="Search Compose services…" />
          <CommandList>
            <CommandEmpty>No Compose service found.</CommandEmpty>
            <CommandGroup heading="Services in the effective Compose file">
              {services.map((service) => (
                <CommandItem
                  key={service.name}
                  value={`${service.name} ${service.image || ''}`}
                  onSelect={() => {
                    onValueChange(service.name)
                    setOpen(false)
                  }}
                >
                  <Check
                    className={cn(
                      'size-4 shrink-0',
                      value === service.name
                        ? 'stroke-foreground'
                        : 'stroke-transparent'
                    )}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-mono">{service.name}</div>
                    <div className="truncate text-muted-foreground">
                      {service.image || 'Image defined at build time'}
                    </div>
                  </div>
                  {service.ports.length > 0 && (
                    <div className="shrink-0 font-mono tabular-nums text-muted-foreground">
                      {service.ports.map((port) => port.target).join(', ')}
                    </div>
                  )}
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

// Repo change dialog — wraps the existing repo selector flow without re-implementing it.
/**
 * Extract `owner/name` from a public repository URL. Shared by the initial
 * seed and the input handler so a project that is already connected to a
 * public repo shows the same parse the user would get by typing it.
 */
export function ChangeRepositoryPage({ project, refetch }: GitSettingsProps) {
  const navigate = useNavigate()
  const { connectionId: connectionIdParam, repositoryId: repositoryIdParam } =
    useParams()
  const routeConnectionId = Number(connectionIdParam)
  const routeRepositoryId = Number(repositoryIdParam)
  const selectedConnectionId =
    Number.isInteger(routeConnectionId) && routeConnectionId > 0
      ? routeConnectionId
      : null
  const selectedRepositoryId =
    Number.isInteger(routeRepositoryId) && routeRepositoryId > 0
      ? routeRepositoryId
      : null
  const updateGit = useMutation({ ...updateGitSettingsMutation() })
  const { data: connectionsData } = useQuery({ ...listConnectionsOptions() })
  const { data: providersData } = useQuery({ ...listGitProvidersOptions() })
  const providers = providersData || []
  const connections = connectionsData?.connections ?? []

  // Source mode: a connected provider (pick a repo) or a public URL. Default to
  // the connection flow whenever the user has any connected provider — a project
  // without its own connection (e.g. one being converted from docker/static)
  // must still be able to pick from the connections it DOES have.
  const [selectedMode, setMode] = useState<'connection' | 'public'>(
    selectedConnectionId || project.git_provider_connection_id
      ? 'connection'
      : 'public'
  )
  const [modeTouched, setModeTouched] = useState(false)
  // A public-repo project also has no connection id, so that alone cannot
  // mean "send them to the picker". For projects without a source, prefer the
  // first available authenticated connection until the user chooses a mode.
  const mode: 'connection' | 'public' = selectedConnectionId
    ? 'connection'
    : !modeTouched &&
        !project.git_provider_connection_id &&
        !project.git_url &&
        connections.length > 0
      ? 'connection'
      : selectedMode
  useEffect(() => {
    if (
      mode === 'connection' &&
      !selectedConnectionId &&
      connections.length > 0
    ) {
      navigate(repositoryConnectionPath(project.slug, connections[0].id), {
        replace: true,
      })
    }
  }, [connections, mode, navigate, project.slug, selectedConnectionId])
  const selectedConnection = connections.find(
    (connection) => connection.id === selectedConnectionId
  )
  const selectedProviderType = providers.find(
    (provider) => provider.id === selectedConnection?.provider_id
  )?.provider_type

  const [selectedRepoState, setSelectedRepoState] =
    useState<RepositoryResponse | null>(null)
  const [connectionRepositoryMode, setConnectionRepositoryMode] = useState<
    'synced' | 'manual'
  >('synced')
  const [manualRepositoryReference, setManualRepositoryReference] = useState('')
  const manualRepository = useMemo(
    () => parseRepositoryCoordinates(manualRepositoryReference),
    [manualRepositoryReference]
  )
  const selectedRepositoryQuery = useQuery({
    ...getRepositoryByIdOptions({
      path: { repository_id: selectedRepositoryId || 0 },
    }),
    enabled: !!selectedRepositoryId,
  })
  const selectedRepo = selectedRepositoryId
    ? selectedRepoState?.id === selectedRepositoryId
      ? selectedRepoState
      : (selectedRepositoryQuery.data ?? null)
    : null
  // Seed from the project so "change repository" starts from the repository
  // it is currently connected to, instead of an empty field the user has to
  // retype from memory.
  const initialPublicUrl =
    !project.git_provider_connection_id && project.git_url
      ? project.git_url
      : ''
  const [publicUrl, setPublicUrl] = useState(initialPublicUrl)
  const [parsedPublic, setParsedPublic] = useState<{
    provider: 'github' | 'gitlab'
    owner: string
    name: string
    instanceUrl?: string
  } | null>(() => parsePublicRepositoryUrl(initialPublicUrl))
  // Branch to connect. Seeded from the currently-connected project's branch,
  // and reset whenever the user picks a different repository (below) so the
  // selector never carries a branch name over from an unrelated repo.
  const [branch, setBranch] = useState<string>(project.main_branch || '')
  const [directory, setDirectory] = useState(project.directory || './')
  // Holds the selector's composite `slug::path` key, not a bare slug — a
  // monorepo can expose the same preset at several paths, and a bare slug
  // makes every card sharing it render as selected. Split via
  // `splitPresetSelection` before sending to the API.
  const [preset, setPreset] = useState<string>(
    presetSelectionKey(project.preset, project.directory)
  )
  const [detectedComposeFile, setDetectedComposeFile] = useState<string | null>(
    null
  )

  /**
   * Adopt a selection from the framework selector. The chosen card's path *is*
   * the root directory to build from, so both move together — otherwise the
   * directory input keeps a stale value from whatever was selected before.
   */
  const selectPreset = (value: string) => {
    setPreset(value)
    setDirectory(splitPresetSelection(value).directory)
  }

  // Detect frameworks/presets for the chosen connected repo.
  const presetQuery = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: (selectedRepo as { id?: number })?.id || 0 },
    }),
    enabled: mode === 'connection' && !!(selectedRepo as { id?: number })?.id,
  })
  // ...and for a public URL, which has no repository id to key off. Without
  // this the public path fell back to the full static catalogue, so the user
  // had to know their own stack instead of being told.
  const publicPresetQuery = useQuery({
    ...detectPublicPresetsOptions({
      path: {
        provider: parsedPublic?.provider || 'github',
        owner: parsedPublic?.owner || '',
        repo: parsedPublic?.name || '',
      },
      query: { base_url: parsedPublic?.instanceUrl },
    }),
    enabled: mode === 'public' && !!parsedPublic,
  })
  // Public branch listings don't flag which entry is the default, so the
  // branch selector can't sort/pre-select it without this — fetched
  // separately since repo metadata and branch listing are different endpoints.
  const publicRepositoryQuery = useQuery({
    ...getPublicRepositoryOptions({
      path: {
        provider: parsedPublic?.provider || 'github',
        owner: parsedPublic?.owner || '',
        repo: parsedPublic?.name || '',
      },
      query: { base_url: parsedPublic?.instanceUrl },
    }),
    enabled: mode === 'public' && !!parsedPublic,
  })
  // The public endpoint answers in snake_case; FrameworkSelector reads
  // camelCase. Same mapping the read-only Git settings page performs.
  const publicPresetData = useMemo(() => {
    if (!publicPresetQuery.data?.presets?.length) return null
    return {
      presets: publicPresetQuery.data.presets.map((p: any) => ({
        preset: p.preset,
        presetLabel: p.preset_label,
        exposedPort: p.exposed_port,
        iconUrl: p.icon_url,
        projectType: p.project_type,
        path: p.path,
        composeFiles: p.compose_files,
      })),
    }
  }, [publicPresetQuery.data])
  const detectedPresetData =
    mode === 'public' ? publicPresetData : (presetQuery.data ?? null)
  const detectedPresetLoading =
    mode === 'public'
      ? publicPresetQuery.isLoading || publicPresetQuery.isFetching
      : presetQuery.isLoading
  const detectedPresetError =
    mode === 'public' ? publicPresetQuery.error : presetQuery.error
  useEffect(() => {
    const detectedList =
      (
        detectedPresetData as
          | {
              presets?: {
                preset: string
                path?: string
                composeFiles?: string[]
              }[]
            }
          | undefined
      )?.presets ?? []
    if (!detectedList.length) return
    // Prefer the detected entry that sits at the directory already configured,
    // so auto-detection doesn't silently relocate the build directory to
    // whichever preset happened to be found first. Fall back to the first.
    const currentPath = normalizePresetPath(directory)
    const detected =
      detectedList.find((p) => normalizePresetPath(p.path) === currentPath) ??
      detectedList[0]
    // Carry the detected path into the key. Storing just the slug here was
    // why auto-detected presets highlighted every card with that slug.
    // Detection is remote query state that deliberately initializes the draft.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    selectPreset(presetSelectionKey(detected.preset, detected.path))
    // Detection also reports which compose file it found. Carrying it over
    // means a repo whose file is named `compose.yml` deploys without the user
    // discovering that the default is `docker-compose.yml`.
    if (detected.preset === 'docker-compose') {
      setDetectedComposeFile(detected.composeFiles?.[0] ?? null)
    } else {
      setDetectedComposeFile(null)
    }
  }, [detectedPresetData])

  const back = () => navigate(`/projects/${project.slug}/git`)
  const connectionBasePath = repositoryConnectionBasePath(project.slug)

  const repoToConnect =
    mode === 'public'
      ? parsedPublic
      : connectionRepositoryMode === 'manual'
        ? manualRepository
        : selectedRepo
          ? { owner: selectedRepo.owner, name: selectedRepo.name }
          : null

  const handleConnect = async () => {
    if (!repoToConnect) return
    const body: Record<string, unknown> = {
      repo_owner: repoToConnect.owner,
      repo_name: repoToConnect.name,
      directory: directory || './',
      // The selector's value is a `slug::path` key; the API takes the slug
      // only. Submitting the key verbatim was rejected as "Unknown preset:
      // dockerfile::examples/go-error-tracking".
      preset: splitPresetSelection(preset).preset || project.preset || 'custom',
      main_branch:
        branch ||
        (selectedRepo as { default_branch?: string })?.default_branch ||
        project.main_branch ||
        'main',
    }
    // `composePath` is camelCase on the wire: DockerComposeConfig is
    // #[serde(rename_all = "camelCase")], and an unknown key is dropped
    // silently rather than rejected.
    if (
      splitPresetSelection(preset).preset === 'docker-compose' &&
      detectedComposeFile
    ) {
      body.preset_config = {
        preset: 'docker-compose',
        composePath: detectedComposeFile,
      }
    }
    if (mode === 'public') {
      const providerOrigin =
        parsedPublic?.instanceUrl ??
        (parsedPublic?.provider === 'gitlab'
          ? 'https://gitlab.com'
          : 'https://github.com')
      body.git_url = `${providerOrigin}/${repoToConnect.owner}/${repoToConnect.name}`
      body.is_public_repo = true
      body.git_provider_connection_id = null
    } else {
      body.git_provider_connection_id = selectedConnectionId
      body.is_public_repo = false
    }
    try {
      await updateGit.mutateAsync({
        body: body as any,
        path: { project_id: project.id },
      })
      toast.success('Repository connected')
      refetch()
      back()
    } catch {
      toast.error('Failed to connect repository')
    }
  }

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between gap-2">
        <div>
          <h2 className="text-xl font-semibold">Connect repository</h2>
          <p className="text-sm text-muted-foreground">
            Connect <span className="font-mono">{project.slug}</span> to a Git
            repository.
          </p>
        </div>
        <Button variant="ghost" size="sm" onClick={back}>
          Cancel
        </Button>
      </div>

      {connections.length > 0 && (
        <div className="inline-flex rounded-lg border p-0.5">
          <Button
            variant={mode === 'connection' ? 'secondary' : 'ghost'}
            size="sm"
            onClick={() => {
              setMode('connection')
              setModeTouched(true)
              setSelectedRepoState(null)
              setBranch('')
              navigate(connectionBasePath)
            }}
          >
            Connected provider
          </Button>
          <Button
            variant={mode === 'public' ? 'secondary' : 'ghost'}
            size="sm"
            onClick={() => {
              setMode('public')
              setModeTouched(true)
              setSelectedRepoState(null)
              setBranch('')
              navigate(connectionBasePath)
            }}
          >
            Public URL
          </Button>
        </div>
      )}

      {mode === 'connection' ? (
        <div className="space-y-3 rounded-lg border p-3">
          <div className="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center">
            <p className="shrink-0 text-base font-medium sm:text-sm">
              Git connection
            </p>
            <Tabs
              value={selectedConnectionId?.toString()}
              onValueChange={(value) => {
                const connectionId = Number(value)
                if (connectionId !== selectedConnectionId) {
                  setSelectedRepoState(null)
                  setBranch('')
                  navigate(repositoryConnectionPath(project.slug, connectionId))
                }
              }}
              className="min-w-0"
            >
              <TabsList
                className="h-9 max-w-full justify-start overflow-x-auto p-1"
                aria-label="Git provider connections"
              >
                {connections.map((c) => {
                  const provider = providers.find((p) => p.id === c.provider_id)
                  return (
                    <TabsTrigger
                      key={c.id}
                      value={c.id.toString()}
                      className="h-7 gap-2 px-2.5 py-1"
                      title={`${provider?.name || 'Git'}: ${c.account_name}`}
                    >
                      <ProviderLogo
                        providerType={provider?.provider_type}
                        className="size-4 shrink-0"
                      />
                      {c.account_name}
                    </TabsTrigger>
                  )
                })}
              </TabsList>
            </Tabs>
          </div>

          {selectedConnectionId && (
            <div className="space-y-3 border-t pt-3">
              <div className="inline-flex rounded-lg border p-0.5">
                <Button
                  type="button"
                  variant={
                    connectionRepositoryMode === 'synced'
                      ? 'secondary'
                      : 'ghost'
                  }
                  size="sm"
                  onClick={() => {
                    setConnectionRepositoryMode('synced')
                    setBranch('')
                  }}
                >
                  Browse synced
                </Button>
                <Button
                  type="button"
                  variant={
                    connectionRepositoryMode === 'manual'
                      ? 'secondary'
                      : 'ghost'
                  }
                  size="sm"
                  onClick={() => {
                    setConnectionRepositoryMode('manual')
                    setSelectedRepoState(null)
                    setBranch('')
                    navigate(
                      repositoryConnectionPath(
                        project.slug,
                        selectedConnectionId
                      )
                    )
                  }}
                >
                  Enter manually
                </Button>
              </div>

              {connectionRepositoryMode === 'synced' ? (
                <RepositorySelector
                  connectionId={selectedConnectionId}
                  onSelect={(repo) => {
                    setSelectedRepoState(repo)
                    setBranch(repo?.default_branch || '')
                    navigate(
                      repo
                        ? repositorySelectionPath(
                            project.slug,
                            selectedConnectionId,
                            repo.id
                          )
                        : repositoryConnectionPath(
                            project.slug,
                            selectedConnectionId
                          )
                    )
                  }}
                  selectedRepository={selectedRepo}
                  title=""
                  description=""
                  showAsCard={false}
                  compactMode
                  itemsPerPage={24}
                  providerType={selectedProviderType}
                />
              ) : (
                <Card>
                  <CardContent className="space-y-2 p-4">
                    <Label htmlFor="connected-repository-reference">
                      Repository
                    </Label>
                    <Input
                      id="connected-repository-reference"
                      value={manualRepositoryReference}
                      onChange={(event) => {
                        const nextReference = event.target.value
                        const nextRepository =
                          parseRepositoryCoordinates(nextReference)
                        if (
                          nextRepository?.owner !== manualRepository?.owner ||
                          nextRepository?.name !== manualRepository?.name
                        ) {
                          setBranch('')
                        }
                        setManualRepositoryReference(nextReference)
                      }}
                      placeholder="owner/repository"
                      autoComplete="off"
                    />
                    <p className="text-xs text-muted-foreground">
                      Temps will access this repository with the selected Git
                      connection. It does not need to be synced first.
                    </p>
                    {manualRepositoryReference && !manualRepository && (
                      <p className="text-xs text-destructive">
                        Enter a repository as owner/repository, an HTTPS clone
                        URL, or an SSH clone URL.
                      </p>
                    )}
                  </CardContent>
                </Card>
              )}
            </div>
          )}
          {!selectedConnectionId && (
            <div className="border-t px-1 pt-3 text-sm text-muted-foreground">
              Choose a Git connection to browse its repositories.
            </div>
          )}
        </div>
      ) : (
        <Card>
          <CardContent className="space-y-2 p-6">
            <Label>Public repository URL</Label>
            <Input
              placeholder="https://github.com/owner/repo"
              value={publicUrl}
              onChange={(e) => {
                const url = e.target.value
                setPublicUrl(url)
                const parsed = parsePublicRepositoryUrl(url)
                if (
                  parsed?.owner !== parsedPublic?.owner ||
                  parsed?.name !== parsedPublic?.name
                ) {
                  setBranch('')
                }
                setParsedPublic(parsed)
              }}
            />
            {parsedPublic && (
              <p className="pt-1 text-sm text-muted-foreground">
                Will connect to{' '}
                <span className="font-mono text-foreground">
                  {parsedPublic.owner}/{parsedPublic.name}
                </span>
              </p>
            )}
          </CardContent>
        </Card>
      )}

      {/* Framework + root directory — shown once a repository is chosen. */}
      {repoToConnect && (
        <Card>
          <CardContent className="space-y-4 p-6">
            <div className="space-y-2">
              <Label>Branch</Label>
              <BranchSelector
                repoOwner={repoToConnect.owner}
                repoName={repoToConnect.name}
                connectionId={
                  mode === 'connection'
                    ? (selectedConnectionId ?? undefined)
                    : undefined
                }
                defaultBranch={
                  mode === 'connection'
                    ? selectedRepo?.default_branch
                    : publicRepositoryQuery.data?.default_branch
                }
                gitUrl={
                  mode === 'public' && parsedPublic
                    ? `${parsedPublic.instanceUrl ?? (parsedPublic.provider === 'gitlab' ? 'https://gitlab.com' : 'https://github.com')}/${parsedPublic.owner}/${parsedPublic.name}`
                    : undefined
                }
                value={branch}
                onChange={setBranch}
                onBranchesLoaded={(loaded) => {
                  if (!branch && loaded.length > 0) {
                    setBranch(loaded[0])
                  }
                }}
              />
            </div>
            <FrameworkSelector
              presetData={detectedPresetData as any}
              isLoading={detectedPresetLoading}
              error={detectedPresetError}
              selectedPreset={preset}
              onSelectPreset={selectPreset}
            />
            <div className="space-y-2">
              <Label htmlFor="root-dir">Root directory</Label>
              <Input
                id="root-dir"
                value={directory}
                onChange={(e) => setDirectory(e.target.value)}
                placeholder="./"
              />
              <p className="text-xs text-muted-foreground">
                Subdirectory to build from (for monorepos). Defaults to the
                repository root.
              </p>
            </div>
          </CardContent>
        </Card>
      )}

      {repoToConnect && (
        <div className="flex justify-end">
          <Button
            onClick={handleConnect}
            disabled={updateGit.isPending || !preset}
          >
            {updateGit.isPending && (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            )}
            Connect repository
          </Button>
        </div>
      )}
    </div>
  )
}

export function GitSettings(props: GitSettingsProps) {
  return <GitSettingsInline {...props} view="git" />
}

export function BuildSettings(
  props: GitSettingsProps & { embedded?: boolean }
) {
  return <GitSettingsInline {...props} view="build" />
}

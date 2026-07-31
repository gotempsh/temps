import {
  ProjectResponse,
  RepositoryResponse,
  getRepositoryBranches,
  listRepositoriesByConnection,
} from '@/api/client'
import {
  detectPublicPresetsOptions,
  getRepositoryPresetLiveOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
  reinstallGitlabWebhookMutation,
  updateAutomaticDeployMutation,
  updateGitSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RepositorySelector } from '@/components/repositories/RepositorySelector'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
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
import { Switch } from '@/components/ui/switch'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import { cn } from '@/lib/utils'
import {
  normalizePresetPath,
  presetConfigForSelection,
  presetSelectionKey,
  splitPresetSelection,
} from '@/lib/preset-selection'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  Check,
  FileIcon,
  FolderIcon,
  GitBranchIcon,
  Loader2,
  Plus,
  RefreshCw,
  Trash2,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import FrameworkIcon from '../FrameworkIcon'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { FrameworkSelector } from '../FrameworkSelector'

interface GitSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

function GitSettingsInline({ project, refetch }: GitSettingsProps) {
  const updateGitSettings = useMutation({
    ...updateGitSettingsMutation(),
    meta: { errorTitle: 'Failed to update git settings' },
  })
  const updateAutomaticDeploy = useMutation({
    ...updateAutomaticDeployMutation(),
    meta: { errorTitle: 'Failed to update auto-deploy' },
  })

  // ---------------- Live API data ----------------
  const isPublicRepo = !project?.git_provider_connection_id

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
    isLoading: isLoadingBranches,
    refetch: refetchBranches,
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
  const branches: Array<{
    name: string
    commit_sha: string
    protected: boolean
  }> = (branchesData?.branches as any) || []

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
        provider: 'github',
        owner: project?.repo_owner || '',
        repo: project?.repo_name || '',
      },
    }),
    enabled: isPublicRepo && !!project?.repo_owner && !!project?.repo_name,
  })
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
      overrides.preset === 'nixpacks' &&
      overrides.preset_config === undefined
        ? presetConfigForSelection('nixpacks', presetCfg)
        : (overrides.preset_config ?? presetCfg ?? undefined)
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
    refetch()
  }

  // ---------------- Inline editors ----------------
  const [editing, setEditing] = useState<
    | null
    | 'branch'
    | 'framework'
    | 'directory'
    | 'dockerfile'
    | 'composePath'
    | 'composeOverride'
  >(null)
  const close = () => setEditing(null)

  // Branch editor
  const [branchDraft, setBranchDraft] = useState('')
  useEffect(
    () => setBranchDraft(project.main_branch || ''),
    [project.main_branch]
  )

  // Directory editor
  const [directoryDraft, setDirectoryDraft] = useState('')
  useEffect(
    () => setDirectoryDraft(project.directory || './'),
    [project.directory]
  )

  // Dockerfile path editor
  const [dockerfileDraft, setDockerfileDraft] = useState('')
  useEffect(() => {
    setDockerfileDraft(
      (project?.preset_config as any)?.dockerfilePath || 'Dockerfile'
    )
  }, [project?.preset_config])

  // Compose path editor
  const [composePathDraft, setComposePathDraft] = useState('')
  useEffect(() => {
    setComposePathDraft(
      (project?.preset_config as any)?.composePath || 'docker-compose.yml'
    )
  }, [project?.preset_config])

  // Compose override editor (full-width textarea, explicit save)
  const [overrideDraft, setOverrideDraft] = useState('')
  useEffect(() => {
    setOverrideDraft((project?.preset_config as any)?.composeOverride || '')
  }, [project?.preset_config])

  const presetName = (project.preset || '').toString()
  const isDockerfilePreset = presetName === 'dockerfile'
  const isComposePreset =
    presetName === 'docker-compose' || presetName === 'dockercompose'

  const navigate = useNavigate()
  const goToChangeRepo = () =>
    navigate(`/projects/${project.slug}/git/change-repository`)

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
  if (!project.repo_owner || !project.repo_name) {
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

  const autoDeployOn = project.deployment_config?.automaticDeploy ?? true

  return (
    <div className="space-y-6">
      {/* ----------------- Repository card ----------------- */}
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
              {repositoryData?.description && (
                <p className="text-sm text-muted-foreground line-clamp-2">
                  {repositoryData.description}
                </p>
              )}
              <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                {repositoryData?.pushed_at && (
                  <span>
                    Pushed <TimeAgo date={repositoryData.pushed_at} />
                  </span>
                )}
                {repositoryData?.default_branch && (
                  <span className="flex items-center gap-1">
                    <GitBranchIcon className="size-3" />
                    default{' '}
                    <span className="font-mono text-foreground">
                      {repositoryData.default_branch}
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

      {/* ----------------- Build configuration card ----------------- */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">Build configuration</CardTitle>
          <CardDescription>
            How Temps builds and deploys this project.
          </CardDescription>
        </CardHeader>
        <CardContent className="p-0">
          <ul className="divide-y">
            {/* Branch row */}
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
                  {repositoryData?.default_branch === project.main_branch && (
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
                              {b.name === repositoryData?.default_branch && (
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
                    onClick={() => refetchBranches()}
                    disabled={isLoadingBranches}
                    title="Refresh branches"
                  >
                    {isLoadingBranches ? (
                      <Loader2 className="size-3.5 animate-spin" />
                    ) : (
                      <RefreshCw className="size-3.5" />
                    )}
                  </Button>
                </div>
              }
            />

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
                  <span className="text-sm truncate">{project.preset}</span>
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
                  if (next === (cfg.composePath || 'docker-compose.yml')) {
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
          </ul>
        </CardContent>
      </Card>

      {/* ----------------- Compose advanced — collapsed accordion ----------------- */}
      {isComposePreset && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Compose overrides</CardTitle>
            <CardDescription>
              Advanced YAML override and public port mapping.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <Label className="text-sm font-medium">YAML override</Label>
                {editing !== 'composeOverride' ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setOverrideDraft(
                        (project?.preset_config as any)?.composeOverride || ''
                      )
                      setEditing('composeOverride')
                    }}
                  >
                    Edit
                  </Button>
                ) : (
                  <div className="flex gap-2">
                    <Button variant="ghost" size="sm" onClick={close}>
                      Cancel
                    </Button>
                    <Button
                      size="sm"
                      disabled={updateGitSettings.isPending}
                      onClick={async () => {
                        const cfg = (project.preset_config as any) || {}
                        await saveGitField({
                          preset_config: {
                            ...cfg,
                            preset: 'docker-compose',
                            composeOverride: overrideDraft || undefined,
                          },
                        })
                        toast.success('Override saved')
                        close()
                      }}
                    >
                      {updateGitSettings.isPending && (
                        <Loader2 className="size-3 mr-1 animate-spin" />
                      )}
                      Save
                    </Button>
                  </div>
                )}
              </div>
              {editing === 'composeOverride' ? (
                <textarea
                  value={overrideDraft}
                  onChange={(e) => setOverrideDraft(e.target.value)}
                  className="flex min-h-[160px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                  placeholder={`services:\n  app:\n    ports:\n      - "8080:80"`}
                />
              ) : (project.preset_config as any)?.composeOverride ? (
                <pre className="p-3 rounded-md border bg-muted/50 text-xs font-mono whitespace-pre-wrap overflow-x-auto">
                  {(project.preset_config as any).composeOverride}
                </pre>
              ) : (
                <p className="text-sm text-muted-foreground italic">
                  No override applied. The compose file is used as-is.
                </p>
              )}
            </div>

            <PublicPortsInline project={project} saveGitField={saveGitField} />
          </CardContent>
        </Card>
      )}

      {/* ----------------- Deployment behavior ----------------- */}
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
                      We couldn't install the webhook automatically. Your GitLab
                      user needs the Maintainer role on{' '}
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
  const cfg: any = (project.preset_config as any) || {}
  const ports: { service: string; port: number }[] =
    cfg.publicPorts || cfg.public_ports || []
  const [draft, setDraft] = useState<{ service: string; port: number }[]>(ports)
  const [dirty, setDirty] = useState(false)
  useEffect(() => {
    setDraft(cfg.publicPorts || cfg.public_ports || [])
    setDirty(false)
  }, [project.preset_config])

  const update = (next: { service: string; port: number }[]) => {
    setDraft(next)
    setDirty(true)
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <div>
          <Label className="text-sm font-medium">Public ports</Label>
          <p className="text-xs text-muted-foreground mt-0.5">
            Ports exposed publicly through the proxy. Other ports stay private.
          </p>
        </div>
        <div className="flex gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => update([...draft, { service: '', port: 0 }])}
          >
            <Plus className="size-3.5 mr-1" />
            Add
          </Button>
        </div>
      </div>

      {draft.length === 0 ? (
        <p className="text-xs text-muted-foreground italic py-2">
          No public ports configured.
        </p>
      ) : (
        <div className="space-y-2">
          {draft.map((row, i) => (
            <div key={i} className="flex items-center gap-2">
              <Input
                value={row.service}
                placeholder="Service name"
                className="flex-1 text-sm"
                onChange={(e) => {
                  const next = [...draft]
                  next[i] = { ...next[i], service: e.target.value }
                  update(next)
                }}
              />
              <Input
                type="number"
                value={row.port || ''}
                placeholder="Port"
                className="w-24 text-sm"
                onChange={(e) => {
                  const next = [...draft]
                  next[i] = { ...next[i], port: Number(e.target.value) }
                  update(next)
                }}
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="size-8 shrink-0"
                onClick={() => update(draft.filter((_, j) => j !== i))}
              >
                <Trash2 className="size-3.5 text-muted-foreground" />
              </Button>
            </div>
          ))}
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
            onClick={async () => {
              const filtered = draft.filter((p) => p.service && p.port > 0)
              await saveGitField({
                preset_config: {
                  ...cfg,
                  preset: 'docker-compose',
                  publicPorts: filtered.length ? filtered : undefined,
                },
              })
              toast.success('Public ports saved')
              setDirty(false)
            }}
          >
            Save ports
          </Button>
        </div>
      )}
    </div>
  )
}

// Repo change dialog — wraps the existing repo selector flow without re-implementing it.
export function ChangeRepositoryPage({ project, refetch }: GitSettingsProps) {
  const navigate = useNavigate()
  const updateGit = useMutation({ ...updateGitSettingsMutation() })
  const { data: connectionsData } = useQuery({ ...listConnectionsOptions() })
  const { data: providersData } = useQuery({ ...listGitProvidersOptions() })
  const providers = providersData || []
  const connections = connectionsData?.connections ?? []

  // Source mode: a connected provider (pick a repo) or a public URL. Default to
  // the connection flow whenever the user has any connected provider — a project
  // without its own connection (e.g. one being converted from docker/static)
  // must still be able to pick from the connections it DOES have.
  const [mode, setMode] = useState<'connection' | 'public'>(
    project.git_provider_connection_id ? 'connection' : 'public'
  )
  const [modeTouched, setModeTouched] = useState(false)
  useEffect(() => {
    if (
      !modeTouched &&
      !project.git_provider_connection_id &&
      connections.length > 0
    ) {
      setMode('connection')
    }
  }, [modeTouched, connections.length, project.git_provider_connection_id])

  const [selectedConnectionId, setSelectedConnectionId] = useState<
    number | null
  >(project.git_provider_connection_id || null)
  useEffect(() => {
    if (
      mode === 'connection' &&
      !selectedConnectionId &&
      connections.length > 0
    ) {
      setSelectedConnectionId(connections[0].id)
    }
  }, [mode, selectedConnectionId, connections])

  const [selectedRepo, setSelectedRepo] = useState<RepositoryResponse | null>(
    null
  )
  const [publicUrl, setPublicUrl] = useState('')
  const [parsedPublic, setParsedPublic] = useState<{
    owner: string
    name: string
  } | null>(null)
  const [directory, setDirectory] = useState(project.directory || './')
  // Holds the selector's composite `slug::path` key, not a bare slug — a
  // monorepo can expose the same preset at several paths, and a bare slug
  // makes every card sharing it render as selected. Split via
  // `splitPresetSelection` before sending to the API.
  const [preset, setPreset] = useState<string>(
    presetSelectionKey(project.preset, project.directory)
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
  useEffect(() => {
    const detectedList =
      (
        presetQuery.data as
          { presets?: { preset: string; path?: string }[] } | undefined
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
    selectPreset(presetSelectionKey(detected.preset, detected.path))
  }, [presetQuery.data])

  const back = () => navigate(`/projects/${project.slug}/git`)

  const repoToConnect =
    mode === 'public'
      ? parsedPublic
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
        (selectedRepo as { default_branch?: string })?.default_branch ||
        project.main_branch ||
        'main',
    }
    if (mode === 'public') {
      body.git_url = `https://github.com/${repoToConnect.owner}/${repoToConnect.name}`
      body.is_public_repo = true
      body.git_provider_connection_id = null
    } else {
      body.git_provider_connection_id = selectedConnectionId
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
    <div className="space-y-6">
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
            }}
          >
            Public URL
          </Button>
        </div>
      )}

      {mode === 'connection' ? (
        <div className="space-y-4">
          <Card>
            <CardContent className="space-y-2 p-6">
              <Label>Git provider connection</Label>
              <Select
                value={selectedConnectionId?.toString()}
                onValueChange={(v) => {
                  setSelectedConnectionId(Number(v))
                  setSelectedRepo(null)
                }}
              >
                <SelectTrigger>
                  <SelectValue placeholder="Select a connection..." />
                </SelectTrigger>
                <SelectContent>
                  {connections.map((c) => {
                    const provider = providers.find(
                      (p) => p.id === c.provider_id
                    )
                    return (
                      <SelectItem key={c.id} value={c.id.toString()}>
                        <div className="flex items-center gap-2">
                          <GithubIcon className="size-4" />
                          {c.account_name}
                          {provider && (
                            <Badge variant="secondary" className="ml-1 text-xs">
                              {provider.name}
                            </Badge>
                          )}
                        </div>
                      </SelectItem>
                    )
                  })}
                </SelectContent>
              </Select>
            </CardContent>
          </Card>

          {selectedConnectionId && (
            <RepositorySelector
              connectionId={selectedConnectionId}
              onSelect={(repo) => setSelectedRepo(repo)}
              selectedRepository={selectedRepo}
              title="Select repository"
              description="Choose a repository from the connected provider."
              showAsCard
            />
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
                const m = url
                  .trim()
                  .match(/(?:github\.com|gitlab\.com)[/:]([^/\s]+)\/([^/\s.]+)/)
                setParsedPublic(
                  m ? { owner: m[1], name: m[2].replace(/\.git$/, '') } : null
                )
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
              <Label>Framework / preset</Label>
              <FrameworkSelector
                presetData={presetQuery.data as any}
                isLoading={presetQuery.isLoading}
                error={presetQuery.error}
                selectedPreset={preset}
                onSelectPreset={selectPreset}
              />
            </div>
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
  return <GitSettingsInline {...props} />
}

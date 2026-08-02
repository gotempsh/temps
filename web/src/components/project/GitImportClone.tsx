import { useEffect, useState, useCallback } from 'react'
import { useNavigate, useSearchParams } from 'react-router'
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  createProjectMutation,
  getPublicBranchesOptions,
  detectPublicPresetsOptions,
  listProjectTemplatesOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectItem,
  SelectContent,
} from '@/components/ui/select'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import { RepositoryList } from '@/components/repositories/RepositoryList'
import { TemplateList, TemplateConfigurator } from '@/components/templates'
import { ManualProjectConfigurator } from '@/components/project/ManualProjectConfigurator'
import type {
  RepositoryResponse,
  TemplateResponse,
} from '@/api/client/types.gen'
import {
  ChevronLeft,
  Link as LinkIcon,
  Loader2,
  FolderGit2,
  Plus,
} from 'lucide-react'
import Github from '@/icons/Github'
import Gitlab from '@/icons/Gitlab'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { NewProjectShell } from '@/components/project/NewProjectShell'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'

type ProjectSource = 'templates' | 'browse' | 'git-url' | 'manual'

const SOURCE_VALUES: ProjectSource[] = ['templates', 'browse', 'git-url', 'manual']

function isProjectSource(value: string | null): value is ProjectSource {
  return value !== null && (SOURCE_VALUES as string[]).includes(value)
}

/** Parsed git URL info for public repositories */
interface ParsedGitUrl {
  provider: 'github' | 'gitlab'
  owner: string
  repo: string
}

/**
 * Parse a git URL to extract provider, owner, and repo name
 * Supports: https://github.com/owner/repo, https://gitlab.com/owner/repo, etc.
 */
function parseGitUrl(url: string): ParsedGitUrl | null {
  try {
    // Clean up the URL
    const cleanUrl = url.trim().replace(/\.git$/, '')

    // Try to parse as URL
    let hostname: string
    let pathname: string

    if (cleanUrl.startsWith('http://') || cleanUrl.startsWith('https://')) {
      const parsed = new URL(cleanUrl)
      hostname = parsed.hostname.toLowerCase()
      pathname = parsed.pathname
    } else if (cleanUrl.includes('@') && cleanUrl.includes(':')) {
      // SSH URL format: git@github.com:owner/repo
      const match = cleanUrl.match(/@([^:]+):(.+)/)
      if (!match) return null
      hostname = match[1].toLowerCase()
      pathname = '/' + match[2]
    } else {
      return null
    }

    // Determine provider
    let provider: 'github' | 'gitlab'
    if (hostname.includes('github')) {
      provider = 'github'
    } else if (hostname.includes('gitlab')) {
      provider = 'gitlab'
    } else {
      return null
    }

    // Extract owner and repo from pathname
    const parts = pathname.split('/').filter(Boolean)
    if (parts.length < 2) return null

    return {
      provider,
      owner: parts[0],
      repo: parts[1],
    }
  } catch {
    return null
  }
}

interface GitImportCloneProps {
  mode?: 'navigation' | 'inline'
  onProjectCreated?: () => void
}

export function GitImportClone({
  mode = 'navigation',
  onProjectCreated,
}: GitImportCloneProps) {
  // In navigation mode, the chosen source is mirrored to a `?source=` query
  // param so browser back/forward and link sharing work. Inline mode (used
  // inside the onboarding flow) keeps the source in plain React state since
  // it doesn't own the URL.
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [localSource, setLocalSource] = useState<ProjectSource | null>(null)

  const selectedSource: ProjectSource | null =
    mode === 'navigation'
      ? isProjectSource(searchParams.get('source'))
        ? (searchParams.get('source') as ProjectSource)
        : null
      : localSource

  const setSelectedSource = useCallback(
    (next: ProjectSource | null) => {
      if (mode === 'navigation') {
        setSearchParams(
          (prev) => {
            const params = new URLSearchParams(prev)
            if (next) {
              params.set('source', next)
            } else {
              params.delete('source')
            }
            // Drop sub-keys belonging to the previous source so we never end
            // up with `?source=git-url&template=foo` style stale state.
            params.delete('template')
            params.delete('repo')
            return params
          },
          { replace: false }
        )
      } else {
        setLocalSource(next)
      }
    },
    [mode, setSearchParams]
  )

  const [selectedConnection, setSelectedConnection] = useState<
    string | undefined
  >()
  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [selectedTemplate, setSelectedTemplate] =
    useState<TemplateResponse | null>(null)
  const [gitUrl, setGitUrl] = useState('')
  const [useGitUrl, setUseGitUrl] = useState(false)
  const [parsedPublicRepo, setParsedPublicRepo] = useState<ParsedGitUrl | null>(
    null
  )
  const [isValidatingUrl, setIsValidatingUrl] = useState(false)
  const [isInitialLoad, setIsInitialLoad] = useState(true)

  // When the URL `source` param changes (e.g. user hits browser back), clear
  // any local state that belongs to a different source so we land on the
  // correct sub-screen instead of leaving stale configurators visible.
  useEffect(() => {
    if (mode !== 'navigation') return
    if (selectedSource !== 'templates' && selectedTemplate) {
      setSelectedTemplate(null)
    }
    if (selectedSource !== 'browse' && selectedSource !== 'git-url') {
      if (selectedRepository) setSelectedRepository(null)
      if (useGitUrl) setUseGitUrl(false)
      if (parsedPublicRepo) setParsedPublicRepo(null)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedSource, mode])

  // Templates fetched at this level so we can resolve `?template=<slug>` from
  // the URL into a `TemplateResponse` (used to hydrate the configurator on
  // page load / browser back-forward / shared link). Also fetched on the
  // entry screen (selectedSource === null) to show a real template count.
  const { data: templatesData } = useQuery({
    ...listProjectTemplatesOptions(),
    enabled:
      mode === 'navigation' &&
      (selectedSource === 'templates' || selectedSource === null),
  })

  const templateSlugFromUrl = mode === 'navigation' ? searchParams.get('template') : null
  const repoUrlFromUrl = mode === 'navigation' ? searchParams.get('repo') : null

  // Hydrate `selectedTemplate` from the URL slug once templates load. Also
  // clears selection when the URL slug is removed (browser back).
  useEffect(() => {
    if (mode !== 'navigation') return
    if (!templateSlugFromUrl) {
      if (selectedTemplate) setSelectedTemplate(null)
      return
    }
    if (selectedTemplate?.slug === templateSlugFromUrl) return
    const match = templatesData?.templates?.find((t) => t.slug === templateSlugFromUrl)
    if (match) setSelectedTemplate(match)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [templateSlugFromUrl, templatesData, mode])

  // Helper to push a URL update with both `source` and an optional sub-key.
  const updateSearchParams = useCallback(
    (updates: Record<string, string | null>) => {
      setSearchParams(
        (prev) => {
          const params = new URLSearchParams(prev)
          for (const [key, value] of Object.entries(updates)) {
            if (value === null || value === '') {
              params.delete(key)
            } else {
              params.set(key, value)
            }
          }
          return params
        },
        { replace: false }
      )
    },
    [setSearchParams]
  )

  // Wrapper that mirrors template selection to the URL in navigation mode.
  const selectTemplate = useCallback(
    (template: TemplateResponse | null) => {
      if (mode === 'navigation') {
        updateSearchParams({ template: template?.slug ?? null })
      } else {
        setSelectedTemplate(template)
      }
    },
    [mode, updateSearchParams]
  )

  const { data: connections } = useQuery({
    ...listConnectionsOptions(),
  })

  // Providers list lets us pick the right icon per connection (a GitLab
  // connection should not render the GitHub mark).
  const { data: gitProviders } = useQuery({
    ...listGitProvidersOptions(),
  })

  const providerTypeForConnectionId = (providerId: number): string | undefined =>
    gitProviders?.find((p) => p.id === providerId)?.provider_type

  const renderProviderIcon = (
    providerId: number | undefined | null,
    className = 'h-4 w-4'
  ) => (
    <ProviderLogo
      providerType={
        providerId != null ? providerTypeForConnectionId(providerId) : undefined
      }
      className={className}
    />
  )

  // Optional `?connection=<id>` param lets callers deep-link straight to a
  // specific connection's repository list — used by the first-run "connect a
  // Git provider" happy path, which sends the user here right after creating a
  // PAT connection so they continue to repo selection without a detour.
  const connectionIdFromUrl =
    mode === 'navigation' ? searchParams.get('connection') : null

  // Land directly on something deployable — the method chooser is not a
  // destination. With a Git connection, that's the provider's repository
  // list; without one, the template gallery (one-click deploys that need no
  // connection). The Repositories pill keeps the connect path reachable.
  useEffect(() => {
    if (selectedSource !== null) return
    if (!connections) return
    const landing =
      connections.connections.length > 0 ? 'browse' : 'templates'
    if (mode === 'navigation') {
      setSearchParams(
        (prev) => {
          const params = new URLSearchParams(prev)
          params.set('source', landing)
          return params
        },
        // replace, not push: Back should leave the page, not bounce through
        // the redirect.
        { replace: true }
      )
    } else {
      setLocalSource(landing)
    }
  }, [connections, selectedSource, mode, setSearchParams])

  useEffect(() => {
    if (!connections || connections.connections.length === 0) return

    // If the URL names a connection that has since appeared in the list, snap
    // to it — even after the initial load. A PAT connection created moments ago
    // may not be in `listConnections` on the first render, so we wait for it
    // rather than getting stuck on the fallback first connection.
    if (connectionIdFromUrl) {
      const preferred = connections.connections.find(
        (c) => c.id.toString() === connectionIdFromUrl
      )
      if (preferred && selectedConnection !== connectionIdFromUrl) {
        queueMicrotask(() => {
          setSelectedConnection(preferred.id.toString())
          setIsInitialLoad(false)
        })
        return
      }
    }

    // Default: select the first connection once, on initial load.
    if (!selectedConnection && isInitialLoad) {
      queueMicrotask(() => {
        setSelectedConnection(connections.connections[0].id.toString())
        setIsInitialLoad(false)
      })
    }
  }, [connections, selectedConnection, isInitialLoad, connectionIdFromUrl])

  // Parse owner/repo from full_name
  const [owner, repo] = (selectedRepository?.full_name || '/').split('/')

  // Note: Public repository info is fetched in handleGitUrlSubmit instead of using a query
  // to have better control over the loading state and error handling

  // Query for branches from authenticated connection
  const { data: authenticatedBranches } = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: owner || '',
        repo: repo || '',
      },
      query: {
        connection_id: Number(selectedConnection),
      },
    }),
    enabled:
      !useGitUrl &&
      !!selectedRepository &&
      !!selectedConnection &&
      !!owner &&
      !!repo,
  })

  // Query for branches from public repository
  const { data: publicBranches } = useQuery({
    ...getPublicBranchesOptions({
      path: {
        provider: parsedPublicRepo?.provider || 'github',
        owner: parsedPublicRepo?.owner || '',
        repo: parsedPublicRepo?.repo || '',
      },
    }),
    enabled: useGitUrl && !!parsedPublicRepo && !!selectedRepository,
  })

  // Use the appropriate branches based on whether it's a public repo
  const branches = useGitUrl ? publicBranches : authenticatedBranches

  // Query for presets from authenticated connection
  const { data: authenticatedPresetData } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: {
        repository_id: selectedRepository?.id || 0,
      },
    }),
    enabled: !useGitUrl && !!selectedRepository && !!selectedRepository?.id,
  })

  // Query for presets from public repository
  const { data: publicPresetData } = useQuery({
    ...detectPublicPresetsOptions({
      path: {
        provider: parsedPublicRepo?.provider || 'github',
        owner: parsedPublicRepo?.owner || '',
        repo: parsedPublicRepo?.repo || '',
      },
      query: {
        branch: selectedRepository?.default_branch,
      },
    }),
    enabled: useGitUrl && !!parsedPublicRepo && !!selectedRepository,
  })

  // Transform public preset data to match ProjectPresetResponse format (camelCase)
  const presetData = useGitUrl
    ? publicPresetData?.presets?.map((p) => ({
        preset: p.preset,
        presetLabel: p.preset_label,
        exposedPort: p.exposed_port,
        iconUrl: p.icon_url,
        projectType: p.project_type,
        path: p.path,
        composeFiles: (p as any).compose_files as string[] | undefined,
      }))
    : authenticatedPresetData?.presets

  const createProjectMutationM = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      toast.success('Project created successfully')
      onProjectCreated?.()
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  /**
   * Validates a public git URL and, on success, populates `selectedRepository`
   * (showing the configurator). Called both from the form's submit button and
   * from a hydration effect when the page is loaded with `?repo=<url>` in the
   * search params.
   *
   * Defined before the early returns below so this `useCallback` always runs
   * (otherwise React throws "Rendered fewer hooks than expected" the first
   * time `selectedTemplate` or `selectedRepository` triggers an early return).
   */
  const validateAndSelectGitUrl = useCallback(
    async (
      urlOverride?: string,
      options: { silent?: boolean; pushToUrl?: boolean } = {}
    ) => {
      const url = (urlOverride ?? gitUrl).trim()
      if (!url) {
        toast.error('Please enter a git URL')
        return
      }

      const parsed = parseGitUrl(url)
      if (!parsed) {
        toast.error(
          'Invalid git URL. Please use a GitHub or GitLab repository URL.'
        )
        return
      }

      setParsedPublicRepo(parsed)
      setIsValidatingUrl(true)

      try {
        const response = await fetch(
          `/api/git/public/${parsed.provider}/${parsed.owner}/${parsed.repo}`
        )

        if (!response.ok) {
          if (response.status === 404) {
            toast.error('Repository not found or is not public')
          } else if (response.status === 429) {
            toast.error('Rate limit exceeded. Please try again later.')
          } else {
            toast.error('Failed to fetch repository information')
          }
          setParsedPublicRepo(null)
          return
        }

        const repoInfo = await response.json()

        const repoFromApi: RepositoryResponse = {
          id: 0,
          name: repoInfo.name,
          full_name: repoInfo.full_name,
          owner: repoInfo.owner,
          private: false,
          default_branch: repoInfo.default_branch,
          description: repoInfo.description,
          language: repoInfo.language,
          clone_url: url,
          ssh_url: null,
          created_at: new Date().toISOString(),
          pushed_at: new Date().toISOString(),
          updated_at: new Date().toISOString(),
          preset: null,
          stars: repoInfo.stars,
          forks: repoInfo.forks,
        } as RepositoryResponse & { stars?: number; forks?: number }

        if (urlOverride) setGitUrl(url)
        setSelectedRepository(repoFromApi)
        setUseGitUrl(true)
        if (!options.silent) {
          toast.success(`Found repository: ${repoInfo.full_name}`)
        }
        if (options.pushToUrl && mode === 'navigation') {
          updateSearchParams({ repo: url })
        }
      } catch (error) {
        toast.error('Failed to validate repository URL')
        setParsedPublicRepo(null)
      } finally {
        setIsValidatingUrl(false)
      }
    },
    [gitUrl, mode, updateSearchParams]
  )

  // Hydrate `selectedRepository` from `?repo=<url>` on mount or browser back.
  // Must live before the early returns so the hook order stays consistent.
  useEffect(() => {
    if (mode !== 'navigation') return
    if (selectedSource !== 'git-url') return
    if (!repoUrlFromUrl) return
    if (selectedRepository && useGitUrl && gitUrl === repoUrlFromUrl) return
    void validateAndSelectGitUrl(repoUrlFromUrl, { silent: true })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repoUrlFromUrl, selectedSource, mode])

  const handleRepositoryClick = (repo: RepositoryResponse) => {
    if (mode === 'navigation') {
      // Navigation mode: navigate to import page
      if (!repo.id) {
        toast.error('Repository is missing an id; cannot import')
        return
      }
      navigate(`/projects/import/${repo.id}`)
    } else {
      // Inline mode: show configurator
      setSelectedRepository(repo)
    }
  }

  // Show TemplateConfigurator when a template is selected — inside the same
  // shell (header + pills) as the picker, so configuring never swaps the
  // page frame.
  if (selectedTemplate) {
    return (
      <NewProjectShell
        activeSource="templates"
        onSelectSource={setSelectedSource}
      >
        <div className="space-y-6">
          <div className="flex items-center gap-4">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => selectTemplate(null)}
            >
              <ChevronLeft className="h-4 w-4 mr-2" />
              Back to Templates
            </Button>
          </div>

          <TemplateConfigurator
            template={selectedTemplate}
            onCancel={() => selectTemplate(null)}
            onSuccess={onProjectCreated}
          />
        </div>
      </NewProjectShell>
    )
  }

  // Show ProjectConfigurator when:
  // 1. In inline mode with authenticated repo selected, OR
  // 2. Using Git URL with public repo selected (works in both modes)
  if (
    selectedRepository &&
    ((mode === 'inline' && selectedConnection) || useGitUrl)
  ) {
    const goBackFromRepo = () => {
      setSelectedRepository(null)
      setUseGitUrl(false)
      setParsedPublicRepo(null)
      if (mode === 'navigation') {
        // Drop `?repo=` but keep `?source=git-url` so the user lands on the
        // URL form, not the picker.
        updateSearchParams({ repo: null })
      } else {
        setSelectedSource(null)
      }
    }
    return (
      <NewProjectShell
        activeSource={useGitUrl ? 'git-url' : 'browse'}
        onSelectSource={setSelectedSource}
      >
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="sm" onClick={goBackFromRepo}>
            <ChevronLeft className="h-4 w-4 mr-2" />
            {useGitUrl ? 'Back to Git URL' : 'Back to Create Project'}
          </Button>
        </div>

        <ProjectConfigurator
          repository={{
            id: selectedRepository.id,
            name: selectedRepository.name,
            owner: selectedRepository.owner || owner,
            full_name: selectedRepository.full_name,
            private: selectedRepository.private || false,
            default_branch:
              branches?.branches?.find((b: any) => b.is_default)?.name ||
              selectedRepository.default_branch ||
              'main',
            created_at:
              selectedRepository.created_at || new Date().toISOString(),
            pushed_at: selectedRepository.pushed_at || new Date().toISOString(),
            updated_at:
              selectedRepository.updated_at || new Date().toISOString(),
            git_provider_connection_id:
              selectedRepository.git_provider_connection_id ??
              Number(selectedConnection) ??
              0,
          }}
          connectionId={useGitUrl ? undefined : Number(selectedConnection)}
          presetData={presetData}
          branches={branches?.branches}
          mode="wizard"
          onSubmit={async (data) => {
            try {
              await createProjectMutationM.mutateAsync({
                body: {
                  name: data.name,
                  preset: data.preset,
                  directory: data.rootDirectory,
                  main_branch: data.branch,
                  repo_name: selectedRepository.name || '',
                  repo_owner: selectedRepository.owner || owner || '',
                  git_url: useGitUrl ? gitUrl : undefined,
                  git_provider_connection_id: useGitUrl
                    ? undefined
                    : Number(selectedConnection),
                  is_public_repo: useGitUrl ? true : undefined,
                  project_type: data.preset === 'custom' ? 'static' : undefined,
                  automatic_deploy: data.autoDeploy,
                  storage_service_ids: data.storageServices || [],
                  environment_variables: data.environmentVariables?.map(
                    (env) => [env.key, env.value] as [string, string]
                  ),
                  preset_config:
                    data.preset === 'dockerfile' && data.dockerfilePath
                      ? {
                          dockerfilePath: data.dockerfilePath,
                        }
                      : data.preset === 'docker-compose'
                        ? {
                            composePath:
                              (data as any).composePath || 'docker-compose.yml',
                          }
                        : undefined,
                  exposed_port:
                    data.preset === 'docker-compose' ? undefined : data.port,
                },
              })
            } catch (error) {
              console.error('Project creation error:', error)
            }
          }}
          onCancel={goBackFromRepo}
        />
      </div>
      </NewProjectShell>
    )
  }

  const handleGitUrlSubmit = () => {
    void validateAndSelectGitUrl(undefined, { pushToUrl: true })
  }

  // Source selection step
  // Entry screen: persistent tab bar + sidebar. Only the content area below
  // the tabs swaps based on `selectedSource` — switching tabs must never
  // navigate away from this shell (that was the bug in the old two-screen
  // "pick a source -> full-width takeover" flow).

  const connectionCount = connections?.connections?.length ?? 0

  const sourceContentEl = (
    <>

        {!selectedSource && !connections && (
          <div className="space-y-3">
            <Skeleton className="h-10 w-full" />
            <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
              {Array.from({ length: 6 }).map((_, i) => (
                <Skeleton key={i} className="h-32 w-full rounded-xl" />
              ))}
            </div>
          </div>
        )}


        {selectedSource === 'templates' && (
          <Card>
            <CardContent className="pt-6">
              <TemplateList
                onTemplateSelect={selectTemplate}
                selectedTemplate={selectedTemplate}
                showFeaturedFirst={true}
              />
            </CardContent>
          </Card>
        )}

        {selectedSource === 'browse' && connections && connectionCount === 0 && (
          <Card>
            <CardContent className="flex flex-col items-center text-center py-12 px-6">
              <FolderGit2 className="size-8 text-muted-foreground mb-3" />
              <p className="text-sm font-medium">No Git provider connected</p>
              <p className="text-xs text-muted-foreground mt-1 mb-4 max-w-xs">
                Connect GitHub or GitLab to browse your repositories, or paste
                a public Git URL instead.
              </p>
              <div className="flex items-center gap-2">
                {mode === 'navigation' && (
                  <Button size="sm" onClick={() => navigate('/git-providers/add')}>
                    <Plus className="h-4 w-4 mr-1.5" />
                    Connect provider
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => setSelectedSource('git-url')}
                >
                  <LinkIcon className="h-4 w-4 mr-1.5" />
                  Use a Git URL
                </Button>
              </div>
            </CardContent>
          </Card>
        )}

        {selectedSource === 'browse' && connectionCount > 0 && (
          <Card>
            <CardContent className="pt-6 space-y-3">
              <Select
                value={selectedConnection}
                onValueChange={setSelectedConnection}
              >
                <SelectTrigger className="w-full">
                  <SelectValue placeholder="Select Connection">
                    {selectedConnection &&
                      connections &&
                      (() => {
                        const selectedConn = connections.connections.find(
                          (c) => c.id.toString() === selectedConnection
                        )
                        return selectedConn ? (
                          <div className="flex items-center gap-2">
                            {renderProviderIcon(selectedConn.provider_id)}
                            <span className="font-medium">
                              {selectedConn.account_name}
                            </span>
                            <span className="text-xs text-muted-foreground">
                              ({selectedConn.account_type})
                            </span>
                          </div>
                        ) : (
                          'Select Connection'
                        )
                      })()}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  {connections?.connections?.map((connection) => (
                    <SelectItem
                      key={connection.id}
                      value={connection.id.toString()}
                    >
                      <div className="flex items-center gap-2">
                        {renderProviderIcon(connection.provider_id)}
                        <span className="font-medium">
                          {connection.account_name}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          ({connection.account_type})
                        </span>
                      </div>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>

              {selectedConnection && (
                <RepositoryList
                  connectionId={Number(selectedConnection)}
                  onRepositorySelect={handleRepositoryClick}
                  showSelection={false}
                  itemsPerPage={15}
                  showHeader={true}
                  compactMode={false}
                />
              )}
            </CardContent>
          </Card>
        )}

        {selectedSource === 'git-url' && (
          <Card>
            <CardContent className="pt-6 space-y-4">
              <div className="space-y-2">
                <Label htmlFor="git-url">Public Repository URL</Label>
                <Input
                  id="git-url"
                  type="url"
                  placeholder="https://github.com/owner/repository"
                  value={gitUrl}
                  onChange={(e) => setGitUrl(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !isValidatingUrl) {
                      handleGitUrlSubmit()
                    }
                  }}
                  disabled={isValidatingUrl}
                />
                <div className="flex items-center gap-4 text-xs text-muted-foreground">
                  <div className="flex items-center gap-1">
                    <Github className="h-3 w-3" />
                    <span>GitHub</span>
                  </div>
                  <div className="flex items-center gap-1">
                    <Gitlab className="h-3 w-3" />
                    <span>GitLab</span>
                  </div>
                  <span className="text-muted-foreground/60">supported</span>
                </div>
              </div>
              <Button
                onClick={handleGitUrlSubmit}
                className="w-full"
                disabled={isValidatingUrl || !gitUrl.trim()}
              >
                {isValidatingUrl ? (
                  <>
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                    Validating repository...
                  </>
                ) : (
                  <>
                    <LinkIcon className="h-4 w-4 mr-2" />
                    Continue with URL
                  </>
                )}
              </Button>

              {/* Show parsed URL preview */}
              {gitUrl &&
                !isValidatingUrl &&
                (() => {
                  const parsed = parseGitUrl(gitUrl)
                  if (parsed) {
                    return (
                      <div className="p-3 bg-muted/50 rounded-md text-sm">
                        <div className="flex items-center gap-2">
                          {parsed.provider === 'github' ? (
                            <Github className="h-4 w-4" />
                          ) : (
                            <Gitlab className="h-4 w-4" />
                          )}
                          <span className="font-medium">
                            {parsed.owner}/{parsed.repo}
                          </span>
                          <Badge variant="secondary" className="text-xs">
                            {parsed.provider}
                          </Badge>
                        </div>
                      </div>
                    )
                  }
                  return null
                })()}
            </CardContent>
          </Card>
        )}

        {selectedSource === 'manual' && (
          <Card>
            <CardContent className="pt-6">
              <ManualProjectConfigurator
                onCancel={() => setSelectedSource(null)}
              />
            </CardContent>
          </Card>
        )}
    </>
  )

  return (
    <NewProjectShell
      activeSource={selectedSource}
      onSelectSource={setSelectedSource}
    >
      {sourceContentEl}
    </NewProjectShell>
  )
}

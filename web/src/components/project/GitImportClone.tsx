import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  createProjectMutation,
  getPublicBranchesOptions,
  detectPublicPresetsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Card, CardHeader, CardTitle, CardContent } from '@/components/ui/card'
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import { RepositoryList } from '@/components/repositories/RepositoryList'
import type { RepositoryResponse } from '@/api/client/types.gen'
import { GitBranch, ChevronLeft, Link as LinkIcon, Loader2, Gitlab } from 'lucide-react'
import Github from '@/icons/Github'
import { toast } from 'sonner'
import { Badge } from '@/components/ui/badge'

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
  const [selectedConnection, setSelectedConnection] = useState<
    string | undefined
  >()
  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [gitUrl, setGitUrl] = useState('')
  const [useGitUrl, setUseGitUrl] = useState(false)
  const [parsedPublicRepo, setParsedPublicRepo] = useState<ParsedGitUrl | null>(null)
  const [isValidatingUrl, setIsValidatingUrl] = useState(false)
  const navigate = useNavigate()
  const [isInitialLoad, setIsInitialLoad] = useState(true)

  const { data: connections } = useQuery({
    ...listConnectionsOptions(),
  })

  useEffect(() => {
    if (
      connections &&
      connections.connections.length > 0 &&
      !selectedConnection &&
      isInitialLoad
    ) {
      queueMicrotask(() => {
        setSelectedConnection(connections.connections[0].id.toString())
        setIsInitialLoad(false)
      })
    }
  }, [connections, selectedConnection, isInitialLoad])

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
    enabled: !useGitUrl && !!selectedRepository && !!selectedConnection && !!owner && !!repo,
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
    ? publicPresetData?.presets?.map(p => ({
        preset: p.preset,
        presetLabel: p.preset_label,
        exposedPort: p.exposed_port,
        iconUrl: p.icon_url,
        projectType: p.project_type,
        path: p.path,
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

  const handleRepositoryClick = (repo: RepositoryResponse) => {
    if (mode === 'navigation') {
      // Navigation mode: navigate to import page
      navigate(
        `/projects/import/${repo.full_name}${selectedConnection ? `?connection_id=${selectedConnection}` : ''}`
      )
    } else {
      // Inline mode: show configurator
      setSelectedRepository(repo)
    }
  }

  // Show ProjectConfigurator when:
  // 1. In inline mode with authenticated repo selected, OR
  // 2. Using Git URL with public repo selected (works in both modes)
  if (
    selectedRepository &&
    ((mode === 'inline' && selectedConnection) || useGitUrl)
  ) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setSelectedRepository(null)
              setUseGitUrl(false)
            }}
          >
            <ChevronLeft className="h-4 w-4 mr-2" />
            Back to {useGitUrl ? 'Git URL' : 'Repositories'}
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
                  git_url: useGitUrl ? gitUrl : '',
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
                },
              })
            } catch (error) {
              console.error('Project creation error:', error)
            }
          }}
          onCancel={() => setSelectedRepository(null)}
        />
      </div>
    )
  }

  const handleGitUrlSubmit = async () => {
    if (!gitUrl.trim()) {
      toast.error('Please enter a git URL')
      return
    }

    // Parse the git URL
    const parsed = parseGitUrl(gitUrl)
    if (!parsed) {
      toast.error('Invalid git URL. Please use a GitHub or GitLab repository URL.')
      return
    }

    setParsedPublicRepo(parsed)
    setIsValidatingUrl(true)

    try {
      // Fetch real repository info from public API
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
        setIsValidatingUrl(false)
        return
      }

      const repoInfo = await response.json()

      // Create repository object from real data
      const repoFromApi: RepositoryResponse = {
        id: 0, // Use 0 for public repos (no database ID)
        name: repoInfo.name,
        full_name: repoInfo.full_name,
        owner: repoInfo.owner,
        private: false,
        default_branch: repoInfo.default_branch,
        description: repoInfo.description,
        language: repoInfo.language,
        clone_url: gitUrl,
        ssh_url: null,
        created_at: new Date().toISOString(),
        pushed_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
        preset: null,
        // Extra fields for display
        stars: repoInfo.stars,
        forks: repoInfo.forks,
      } as RepositoryResponse & { stars?: number; forks?: number }

      setSelectedRepository(repoFromApi)
      setUseGitUrl(true)
      toast.success(`Found repository: ${repoInfo.full_name}`)
    } catch (error) {
      toast.error('Failed to validate repository URL')
      setParsedPublicRepo(null)
    } finally {
      setIsValidatingUrl(false)
    }
  }

  return (
    <Card className="flex-1">
      <CardHeader className="flex items-center gap-2 pb-3">
        <GitBranch className="h-5 w-5 text-foreground" />
        <CardTitle className="text-xl font-bold">
          Import Git Repository
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <Tabs defaultValue="browse" className="w-full">
          <TabsList className="grid w-full grid-cols-2">
            <TabsTrigger value="browse">Browse Repositories</TabsTrigger>
            <TabsTrigger value="git-url">
              <LinkIcon className="h-4 w-4 mr-2" />
              Use Git URL
            </TabsTrigger>
          </TabsList>

          <TabsContent value="browse" className="space-y-3 mt-4">
            <div className="flex flex-col gap-2">
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
                            <Github className="h-4 w-4" />
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
                        <Github className="h-4 w-4" />
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
            </div>

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
          </TabsContent>

          <TabsContent value="git-url" className="space-y-4 mt-4">
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
            {gitUrl && !isValidatingUrl && (() => {
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
                      <span className="font-medium">{parsed.owner}/{parsed.repo}</span>
                      <Badge variant="secondary" className="text-xs">
                        {parsed.provider}
                      </Badge>
                    </div>
                  </div>
                )
              }
              return null
            })()}
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}

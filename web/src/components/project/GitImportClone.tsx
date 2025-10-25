import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  createProjectMutation,
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
import { GitBranch, ChevronLeft, Link as LinkIcon } from 'lucide-react'
import Github from '@/icons/Github'
import { toast } from 'sonner'

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

  const { data: branches } = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: owner || '',
        repo: repo || '',
      },
      query: {
        connection_id: Number(selectedConnection),
      },
    }),
    enabled: !!selectedRepository && !!selectedConnection && !!owner && !!repo,
  })

  const { data: presetData } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: {
        repository_id: selectedRepository?.id || 0,
      },
    }),
    enabled: !!selectedRepository && !!selectedRepository?.id,
  })

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

  // If in inline mode and repository is selected, show ProjectConfigurator
  if (
    mode === 'inline' &&
    selectedRepository &&
    (selectedConnection || useGitUrl)
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

  const handleGitUrlSubmit = () => {
    if (!gitUrl.trim()) {
      toast.error('Please enter a git URL')
      return
    }

    // Extract repository name from URL (e.g., https://github.com/owner/repo.git -> repo)
    const urlParts = gitUrl.replace('.git', '').split('/')
    const repoName = urlParts[urlParts.length - 1]
    const owner = urlParts[urlParts.length - 2]

    // Create a mock repository object for public repo
    const mockRepo: RepositoryResponse = {
      id: 0, // Use 0 for public repos
      name: repoName,
      full_name: `${owner}/${repoName}`,
      owner: owner,
      private: false,
      default_branch: 'main',
      description: null,
      language: null,
      clone_url: gitUrl,
      ssh_url: null,
      created_at: new Date().toISOString(),
      pushed_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      preset: null,
    }

    setSelectedRepository(mockRepo)
    setUseGitUrl(true)
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
                placeholder="https://github.com/owner/repository.git"
                value={gitUrl}
                onChange={(e) => setGitUrl(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    handleGitUrlSubmit()
                  }
                }}
              />
              <p className="text-xs text-muted-foreground">
                Enter the HTTPS URL of a public git repository. For example:{' '}
                <code className="text-xs bg-muted px-1 py-0.5 rounded">
                  https://github.com/vercel/next.js.git
                </code>
              </p>
            </div>
            <Button onClick={handleGitUrlSubmit} className="w-full">
              <LinkIcon className="h-4 w-4 mr-2" />
              Continue with URL
            </Button>
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  )
}

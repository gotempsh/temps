import {
  checkCommitExistsOptions,
  getEnvironmentsOptions,
  getProjectBySlugOptions,
  getRepositoryByNameOptions,
  getTagsByRepositoryIdOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  CommitInfo,
  EnvironmentResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { hashKey, useQuery } from '@tanstack/react-query'
import { useMemo, useState, useEffect } from 'react'
import { toast } from 'sonner'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  AlertTriangle,
  CheckCircle2,
  GitCommitHorizontal,
  Loader2,
  RefreshCw,
  Tag as TagIcon,
} from 'lucide-react'
import { BranchSelector } from './BranchSelector'

const COMMIT_SHA_PATTERN = /^[0-9a-f]{7,40}$/i

function isValidCommitSha(commit: string) {
  return COMMIT_SHA_PATTERN.test(commit.trim())
}

function formatCommitDate(date: string) {
  const parsedDate = new Date(date)
  if (Number.isNaN(parsedDate.getTime())) return date

  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(parsedDate)
}

function CommitDetailsCard({
  commit,
  label,
  reference,
}: {
  commit: CommitInfo
  label: string
  reference?: string
}) {
  return (
    <div className="overflow-hidden rounded-md border bg-muted/20">
      <div className="flex items-center justify-between gap-3 border-b bg-muted/40 px-3 py-2">
        <div className="flex min-w-0 items-center gap-2 text-sm font-medium">
          <CheckCircle2 className="h-4 w-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
          {label}
        </div>
        <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
          {reference && (
            <span className="flex min-w-0 items-center gap-1 font-medium text-foreground">
              <TagIcon className="h-3.5 w-3.5 shrink-0" />
              <span className="max-w-40 truncate" title={reference}>
                {reference}
              </span>
            </span>
          )}
          <span className="flex shrink-0 items-center gap-1 font-mono">
            <GitCommitHorizontal className="h-3.5 w-3.5" />
            {commit.sha.slice(0, 12)}
          </span>
        </div>
      </div>
      <div className="space-y-2.5 px-3 py-3">
        <p className="max-h-24 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed">
          {commit.message}
        </p>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          <span className="font-medium text-foreground">{commit.author}</span>
          <span aria-hidden="true">·</span>
          <span>{formatCommitDate(commit.date)}</span>
          <span aria-hidden="true">·</span>
          <span className="truncate">{commit.author_email}</span>
        </div>
      </div>
    </div>
  )
}

interface RedeploymentModalProps {
  project: ProjectResponse
  isOpen: boolean
  onClose: () => void
  onConfirm: (reference: {
    branch?: string
    commit?: string
    tag?: string
    environmentId: number
  }) => Promise<void>
  defaultBranch?: string
  defaultType?: 'branch' | 'commit' | 'tag'
  defaultEnvironment?: number
  defaultCommit?: string
  defaultTag?: string
  isLoading?: boolean
  mode?: 'new' | 'redeploy' // 'new' = full form, 'redeploy' = simple confirmation
  /**
   * Prebuilt image reference for docker_image projects. When the project's
   * source_type is `docker_image`, the modal shows an image-deploy view
   * (image + environment, no branch/commit/tag) and the parent re-pulls this
   * image via deploy_from_image instead of the git pipeline.
   */
  imageRef?: string | null
}

export function RedeploymentModal({
  project,
  isOpen,
  onClose,
  onConfirm,
  defaultBranch,
  defaultEnvironment,
  defaultCommit,
  defaultTag,
  defaultType,
  isLoading,
  mode = 'new',
  imageRef,
}: RedeploymentModalProps) {
  // Image-based (docker_image) projects deploy a prebuilt image, not a git
  // ref — the parent routes confirmation through deploy_from_image.
  const isImageDeploy = project?.source_type === 'docker_image'
  // Fetch project details to get repo info and main branch
  const projectQuery = useQuery({
    ...getProjectBySlugOptions({
      path: { slug: project?.slug },
    }),
    enabled: !!project?.slug && isOpen,
  })

  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id && isOpen,
  })

  // Compute initial branch value from query data or defaults using useMemo
  const initialBranch = useMemo(() => {
    if (defaultBranch) return defaultBranch
    if (projectQuery.data?.main_branch) return projectQuery.data.main_branch
    return ''
  }, [defaultBranch, projectQuery.data?.main_branch])

  // Compute initial environment value from query data or defaults using useMemo
  const initialEnvironment = useMemo(() => {
    if (defaultEnvironment) return defaultEnvironment
    if (environmentsQuery.data?.length) return environmentsQuery.data[0].id
    return null
  }, [defaultEnvironment, environmentsQuery.data])

  // State variables that use the computed initial values
  const [selectedBranch, setSelectedBranch] = useState('')
  const [selectedEnvironment, setSelectedEnvironment] = useState<number | null>(
    null
  )
  const [selectedCommit, setSelectedCommit] = useState(defaultCommit || '')
  const [selectedTag, setSelectedTag] = useState(defaultTag || '')
  const [deploymentType, setDeploymentType] = useState<
    'branch' | 'commit' | 'tag'
  >(defaultType || 'branch')
  const [availableBranches, setAvailableBranches] = useState<string[]>([])
  const [commitToCheck, setCommitToCheck] = useState('')
  const [tagToCheck, setTagToCheck] = useState('')

  // Derive effective values (either user-selected or initial/default)
  const effectiveBranch = selectedBranch !== '' ? selectedBranch : initialBranch
  const effectiveEnvironment = selectedEnvironment ?? initialEnvironment
  const normalizedCommit = selectedCommit.trim()
  const normalizedTag = selectedTag.trim()
  const commitFormatIsValid = isValidCommitSha(normalizedCommit)
  const tagHasValue = normalizedTag.length > 0
  const branchNotFound = Boolean(
    effectiveBranch &&
    availableBranches.length > 0 &&
    !availableBranches.includes(effectiveBranch)
  )
  const projectDetails = projectQuery.data ?? project
  const shouldLookUpGitReference = Boolean(
    projectDetails.git_provider_connection_id &&
    projectDetails.repo_owner &&
    projectDetails.repo_name
  )

  const repositoryQuery = useQuery({
    ...getRepositoryByNameOptions({
      path: {
        owner: projectDetails.repo_owner || '',
        name: projectDetails.repo_name || '',
      },
      query: {
        connection_id: projectDetails.git_provider_connection_id || 0,
      },
    }),
    enabled:
      isOpen &&
      mode === 'new' &&
      (deploymentType === 'commit' ||
        (deploymentType === 'tag' && tagHasValue)) &&
      shouldLookUpGitReference,
  })

  const commitQuery = useQuery({
    ...checkCommitExistsOptions({
      path: {
        repository_id: repositoryQuery.data?.id || 0,
        commit_sha: commitToCheck,
      },
    }),
    enabled:
      isOpen &&
      deploymentType === 'commit' &&
      !!repositoryQuery.data?.id &&
      !!commitToCheck,
    retry: false,
  })

  const checkedCurrentCommit =
    !!commitToCheck && commitToCheck === normalizedCommit.toLowerCase()
  const commitIsVerified = Boolean(
    checkedCurrentCommit && commitQuery.data?.exists && commitQuery.data.commit
  )

  const tagsQueryOptions = getTagsByRepositoryIdOptions({
    path: { repository_id: repositoryQuery.data?.id || 0 },
    query: { fresh: true },
  })
  const tagsQuery = useQuery({
    ...tagsQueryOptions,
    // The endpoint returns the repository's full tag list, but verification is
    // tied to one user-entered tag. Including it in the client query key forces
    // a new provider-backed check when the tag changes instead of reusing a
    // previously verified list for a different deployment decision.
    queryKeyHashFn: (queryKey) => hashKey([...queryKey, tagToCheck]),
    enabled:
      isOpen &&
      deploymentType === 'tag' &&
      !!repositoryQuery.data?.id &&
      !!tagToCheck,
    retry: false,
    staleTime: 0,
  })

  const checkedCurrentTag =
    !!tagToCheck && tagToCheck === normalizedTag && tagHasValue
  const matchingTag = checkedCurrentTag
    ? tagsQuery.data?.tags.find((tag) => tag.name === tagToCheck)
    : undefined

  const tagCommitQuery = useQuery({
    ...checkCommitExistsOptions({
      path: {
        repository_id: repositoryQuery.data?.id || 0,
        commit_sha: matchingTag?.commit_sha || '',
      },
    }),
    enabled:
      isOpen &&
      deploymentType === 'tag' &&
      !!repositoryQuery.data?.id &&
      !!matchingTag?.commit_sha,
    retry: false,
  })

  const tagIsVerified = Boolean(
    checkedCurrentTag &&
    !tagsQuery.isFetching &&
    matchingTag &&
    !tagCommitQuery.isFetching &&
    tagCommitQuery.data?.exists &&
    tagCommitQuery.data.commit
  )

  // Reset form state when modal opens or default values change
  useEffect(() => {
    if (isOpen) {
      // The dialog is controlled by its parent, so opening it is the boundary
      // where draft selections must be reset to the latest defaults.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSelectedBranch('')
      setSelectedEnvironment(null)
      setSelectedCommit(defaultCommit || '')
      setSelectedTag(defaultTag || '')
      setDeploymentType(defaultType || 'branch')
    }
  }, [isOpen, defaultCommit, defaultTag, defaultType])

  const handleEnvironmentChange = (value: string) => {
    const environmentId = value ? parseInt(value) : null
    setSelectedEnvironment(environmentId)

    if (!environmentId || !environmentsQuery.data || isImageDeploy) return

    const selectedEnv = environmentsQuery.data.find(
      (env: EnvironmentResponse) => env.id === environmentId
    )
    if (selectedEnv?.branch) {
      setDeploymentType('branch')
      setSelectedBranch(selectedEnv.branch)
    }
  }

  // Avoid issuing a provider API request for every character after the
  // minimum SHA length. Pasting or pausing on a syntactically valid SHA
  // resolves the commit details after a short debounce.
  useEffect(() => {
    const canCheckCommit =
      isOpen &&
      deploymentType === 'commit' &&
      shouldLookUpGitReference &&
      commitFormatIsValid
    const nextCommit = canCheckCommit ? normalizedCommit.toLowerCase() : ''

    const timeoutId = window.setTimeout(
      () => {
        setCommitToCheck(nextCommit)
      },
      canCheckCommit ? 350 : 0
    )

    return () => window.clearTimeout(timeoutId)
  }, [
    commitFormatIsValid,
    deploymentType,
    isOpen,
    normalizedCommit,
    shouldLookUpGitReference,
  ])

  // Tag names are matched case-sensitively against the provider's tag list.
  // Debouncing avoids flashing a not-found state while a tag is being typed.
  useEffect(() => {
    const canCheckTag =
      isOpen &&
      deploymentType === 'tag' &&
      shouldLookUpGitReference &&
      tagHasValue
    const nextTag = canCheckTag ? normalizedTag : ''

    const timeoutId = window.setTimeout(
      () => {
        setTagToCheck(nextTag)
      },
      canCheckTag ? 350 : 0
    )

    return () => window.clearTimeout(timeoutId)
  }, [
    deploymentType,
    isOpen,
    normalizedTag,
    shouldLookUpGitReference,
    tagHasValue,
  ])

  const validateCommit = (commit: string) => {
    return isValidCommitSha(commit)
  }

  const handleConfirm = async () => {
    // Image-based projects: only the environment matters; the parent re-pulls
    // the prebuilt image. In redeploy mode the environment is fixed; in new
    // mode the user picks it.
    if (isImageDeploy) {
      const envId =
        mode === 'redeploy' ? defaultEnvironment : effectiveEnvironment
      if (!envId) {
        toast.error('No environment specified for deployment')
        return
      }
      await onConfirm({ environmentId: envId })
      return
    }

    // In redeploy mode, use the default values directly
    if (mode === 'redeploy') {
      if (!defaultEnvironment) {
        toast.error('No environment specified for redeployment')
        return
      }

      await onConfirm({
        branch: defaultType === 'branch' ? defaultBranch : undefined,
        commit:
          defaultType === 'commit' || defaultType === 'tag'
            ? defaultCommit
            : undefined,
        tag: defaultType === 'tag' ? defaultTag : undefined,
        environmentId: defaultEnvironment,
      })
      return
    }

    // In new mode, validate and use selected/effective values
    if (deploymentType === 'commit' && !validateCommit(selectedCommit)) {
      toast.error(
        'Enter a commit hash containing 7 to 40 hexadecimal characters'
      )
      return
    }
    if (
      deploymentType === 'commit' &&
      shouldLookUpGitReference &&
      !commitIsVerified
    ) {
      toast.error('Wait for the commit to be verified before deploying')
      return
    }
    if (deploymentType === 'tag' && !tagHasValue) {
      toast.error('Enter a tag name')
      return
    }
    if (
      deploymentType === 'tag' &&
      shouldLookUpGitReference &&
      !tagIsVerified
    ) {
      toast.error('Wait for the tag to be verified before deploying')
      return
    }
    if (!effectiveEnvironment) {
      return
    }

    const environmentExists = environmentsQuery.data?.some(
      (env: EnvironmentResponse) => env.id === effectiveEnvironment
    )
    if (!environmentExists) {
      toast.error('Invalid environment selected')
      return
    }

    await onConfirm({
      branch: deploymentType === 'branch' ? effectiveBranch : undefined,
      commit:
        deploymentType === 'commit'
          ? normalizedCommit.toLowerCase()
          : deploymentType === 'tag'
            ? tagCommitQuery.data?.commit?.sha.toLowerCase()
            : undefined,
      tag: deploymentType === 'tag' ? normalizedTag : undefined,
      environmentId: effectiveEnvironment,
    })
  }

  // Get environment name for redeploy mode
  const environmentName =
    environmentsQuery.data?.find(
      (env: EnvironmentResponse) => env.id === defaultEnvironment
    )?.name ||
    environmentsQuery.data?.find(
      (env: EnvironmentResponse) => env.id === defaultEnvironment
    )?.slug

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent className="sm:max-w-[640px]">
        <DialogHeader>
          <DialogTitle>
            {mode === 'redeploy' ? 'Redeploy' : 'Deploy Project'}
          </DialogTitle>
        </DialogHeader>

        {/* Image-deploy view (docker_image projects): re-pull the prebuilt
            image; no branch/commit/tag. */}
        {isImageDeploy ? (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              {mode === 'redeploy'
                ? 'This will re-pull and run the prebuilt image:'
                : 'Deploy the prebuilt image:'}
            </p>
            <div className="space-y-3 rounded-md border bg-muted/50 p-4">
              <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-2">
                <div className="text-sm font-medium">Image:</div>
                <div
                  className="truncate text-sm font-mono"
                  title={imageRef || ''}
                >
                  {imageRef || 'N/A'}
                </div>
                {mode === 'redeploy' ? (
                  <>
                    <div className="text-sm font-medium">Environment:</div>
                    <div className="text-sm">
                      {environmentName || 'Loading...'}
                    </div>
                  </>
                ) : null}
              </div>
            </div>
            {mode !== 'redeploy' && (
              <div className="space-y-2">
                <Label htmlFor="image-environment">Environment</Label>
                <Select
                  value={effectiveEnvironment?.toString() || ''}
                  onValueChange={handleEnvironmentChange}
                  disabled={environmentsQuery.isLoading}
                >
                  <SelectTrigger>
                    <SelectValue
                      placeholder={
                        environmentsQuery.isLoading
                          ? 'Loading...'
                          : 'Select environment'
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {environmentsQuery.data?.map((env: EnvironmentResponse) => (
                      <SelectItem key={env.id} value={env.id.toString()}>
                        {env.name || env.slug}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
            <DialogFooter>
              <Button variant="outline" onClick={onClose} disabled={isLoading}>
                Cancel
              </Button>
              <Button
                onClick={handleConfirm}
                disabled={
                  isLoading ||
                  !imageRef ||
                  (mode === 'redeploy'
                    ? !defaultEnvironment
                    : !effectiveEnvironment)
                }
              >
                {isLoading
                  ? mode === 'redeploy'
                    ? 'Redeploying...'
                    : 'Deploying...'
                  : mode === 'redeploy'
                    ? 'Redeploy'
                    : 'Deploy'}
              </Button>
            </DialogFooter>
          </div>
        ) : mode === 'redeploy' ? (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">
              This will redeploy using the following configuration:
            </p>
            <div className="space-y-3 border rounded-md p-4 bg-muted/50">
              <div className="grid grid-cols-2 gap-2">
                <div className="text-sm font-medium">Deploy from:</div>
                <div className="text-sm">
                  {defaultType === 'branch' && (
                    <span className="font-mono">{defaultBranch || 'N/A'}</span>
                  )}
                  {defaultType === 'commit' && (
                    <span className="font-mono">{defaultCommit || 'N/A'}</span>
                  )}
                  {defaultType === 'tag' && (
                    <span className="font-mono">{defaultTag || 'N/A'}</span>
                  )}
                </div>

                <div className="text-sm font-medium">Type:</div>
                <div className="text-sm capitalize">{defaultType}</div>

                {/* Show commit hash if available */}
                {defaultCommit && (
                  <>
                    <div className="text-sm font-medium">Commit:</div>
                    <div className="text-sm font-mono text-muted-foreground">
                      {defaultCommit.substring(0, 7)}
                    </div>
                  </>
                )}

                <div className="text-sm font-medium">Environment:</div>
                <div className="text-sm">{environmentName || 'Loading...'}</div>
              </div>
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={onClose} disabled={isLoading}>
                Cancel
              </Button>
              <Button
                onClick={handleConfirm}
                disabled={isLoading || !defaultEnvironment}
              >
                {isLoading ? 'Redeploying...' : 'Redeploy'}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          /* Full form for new deployment mode */
          <>
            <div className="space-y-4">
              <div className="space-y-2">
                <Label>Deploy from</Label>
                <Tabs
                  value={deploymentType}
                  onValueChange={(v) =>
                    setDeploymentType(v as 'branch' | 'commit' | 'tag')
                  }
                >
                  <TabsList className="grid w-full grid-cols-3">
                    <TabsTrigger value="branch">Branch</TabsTrigger>
                    <TabsTrigger value="commit">Commit</TabsTrigger>
                    <TabsTrigger value="tag">Tag</TabsTrigger>
                  </TabsList>
                  <TabsContent value="branch" className="space-y-2">
                    {branchNotFound && (
                      <Alert className="border-amber-200 bg-amber-50">
                        <AlertTriangle className="h-4 w-4 text-amber-600" />
                        <AlertDescription className="text-amber-800">
                          The branch “{effectiveBranch}” for this environment
                          was not found in the repository. You can continue with
                          the current branch name, or switch to deploy by commit
                          hash.
                        </AlertDescription>
                      </Alert>
                    )}
                    {deploymentType === 'branch' &&
                    selectedEnvironment &&
                    !availableBranches.includes(selectedBranch) &&
                    availableBranches.length > 0 ? (
                      <div className="space-y-2">
                        <Input
                          value={effectiveBranch}
                          onChange={(e) => setSelectedBranch(e.target.value)}
                          placeholder="Enter branch name manually"
                          disabled={isLoading}
                        />
                      </div>
                    ) : (
                      <BranchSelector
                        repoOwner={projectQuery.data?.repo_owner || ''}
                        repoName={projectQuery.data?.repo_name || ''}
                        connectionId={
                          projectQuery.data?.git_provider_connection_id ||
                          undefined
                        }
                        gitUrl={(projectQuery.data as any)?.git_url}
                        defaultBranch={
                          initialBranch || projectQuery.data?.main_branch
                        }
                        value={effectiveBranch}
                        onChange={(branch) => {
                          setSelectedBranch(branch)
                        }}
                        onBranchesLoaded={(branches) =>
                          setAvailableBranches(branches)
                        }
                        disabled={isLoading}
                      />
                    )}
                  </TabsContent>
                  <TabsContent value="commit" className="space-y-3">
                    <div className="space-y-1.5">
                      <Input
                        value={selectedCommit}
                        onChange={(e) => setSelectedCommit(e.target.value)}
                        placeholder="Enter commit hash"
                        spellCheck={false}
                        autoComplete="off"
                        className="font-mono"
                        aria-invalid={
                          normalizedCommit.length > 0 && !commitFormatIsValid
                        }
                        aria-describedby="commit-lookup-status"
                      />
                      {normalizedCommit.length > 0 && !commitFormatIsValid && (
                        <p className="text-xs text-destructive">
                          Use 7 to 40 hexadecimal characters.
                        </p>
                      )}
                    </div>

                    <div id="commit-lookup-status" aria-live="polite">
                      {commitFormatIsValid && shouldLookUpGitReference && (
                        <>
                          {(repositoryQuery.isLoading ||
                            !checkedCurrentCommit ||
                            commitQuery.isLoading) &&
                            !repositoryQuery.isError && (
                              <div className="flex items-center gap-2 rounded-md border bg-muted/30 px-3 py-2.5 text-sm text-muted-foreground">
                                <Loader2 className="h-4 w-4 animate-spin" />
                                Checking commit with the Git provider…
                              </div>
                            )}

                          {(repositoryQuery.isError || commitQuery.isError) && (
                            <Alert variant="destructive">
                              <AlertTriangle className="h-4 w-4" />
                              <AlertDescription className="flex items-center justify-between gap-3">
                                <span>
                                  Commit details could not be loaded. Check the
                                  Git provider connection and try again.
                                </span>
                                <Button
                                  type="button"
                                  variant="outline"
                                  size="sm"
                                  className="shrink-0"
                                  onClick={() => {
                                    if (repositoryQuery.isError) {
                                      void repositoryQuery.refetch()
                                    } else {
                                      void commitQuery.refetch()
                                    }
                                  }}
                                >
                                  <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
                                  Retry
                                </Button>
                              </AlertDescription>
                            </Alert>
                          )}

                          {checkedCurrentCommit &&
                            commitQuery.data &&
                            !commitQuery.data.exists && (
                              <Alert variant="destructive">
                                <AlertTriangle className="h-4 w-4" />
                                <AlertDescription>
                                  This commit was not found in{' '}
                                  <span className="font-medium">
                                    {projectDetails.repo_owner}/
                                    {projectDetails.repo_name}
                                  </span>
                                  .
                                </AlertDescription>
                              </Alert>
                            )}

                          {commitIsVerified && commitQuery.data?.commit && (
                            <CommitDetailsCard
                              commit={commitQuery.data.commit}
                              label="Commit found"
                            />
                          )}
                        </>
                      )}
                    </div>
                  </TabsContent>
                  <TabsContent value="tag" className="space-y-3">
                    <div className="space-y-1.5">
                      <Input
                        value={selectedTag}
                        onChange={(e) => setSelectedTag(e.target.value)}
                        placeholder="Enter tag name"
                        spellCheck={false}
                        autoComplete="off"
                        className="font-mono"
                        aria-invalid={selectedTag.length > 0 && !tagHasValue}
                        aria-describedby="tag-lookup-status"
                      />
                      {selectedTag.length > 0 && !tagHasValue && (
                        <p className="text-xs text-destructive">
                          Enter a non-empty tag name.
                        </p>
                      )}
                    </div>

                    <div id="tag-lookup-status" aria-live="polite">
                      {tagHasValue && shouldLookUpGitReference && (
                        <>
                          {(repositoryQuery.isLoading ||
                            !checkedCurrentTag ||
                            tagsQuery.isFetching ||
                            (!!matchingTag && tagCommitQuery.isFetching)) &&
                            !repositoryQuery.isError &&
                            !tagsQuery.isError &&
                            !tagCommitQuery.isError && (
                              <div className="flex items-center gap-2 rounded-md border bg-muted/30 px-3 py-2.5 text-sm text-muted-foreground">
                                <Loader2 className="h-4 w-4 animate-spin" />
                                Checking tag with the Git provider…
                              </div>
                            )}

                          {(repositoryQuery.isError ||
                            tagsQuery.isError ||
                            tagCommitQuery.isError) && (
                            <Alert variant="destructive">
                              <AlertTriangle className="h-4 w-4" />
                              <AlertDescription className="flex items-center justify-between gap-3">
                                <span>
                                  Tag details could not be loaded. Check the Git
                                  provider connection and try again.
                                </span>
                                <Button
                                  type="button"
                                  variant="outline"
                                  size="sm"
                                  className="shrink-0"
                                  onClick={() => {
                                    if (repositoryQuery.isError) {
                                      void repositoryQuery.refetch()
                                    } else if (tagsQuery.isError) {
                                      void tagsQuery.refetch()
                                    } else {
                                      void tagCommitQuery.refetch()
                                    }
                                  }}
                                >
                                  <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
                                  Retry
                                </Button>
                              </AlertDescription>
                            </Alert>
                          )}

                          {checkedCurrentTag &&
                            tagsQuery.data &&
                            !matchingTag && (
                              <Alert variant="destructive">
                                <AlertTriangle className="h-4 w-4" />
                                <AlertDescription>
                                  Tag “{normalizedTag}” was not found in{' '}
                                  <span className="font-medium">
                                    {projectDetails.repo_owner}/
                                    {projectDetails.repo_name}
                                  </span>
                                  . Tag names are case-sensitive.
                                </AlertDescription>
                              </Alert>
                            )}

                          {checkedCurrentTag &&
                            matchingTag &&
                            tagCommitQuery.data &&
                            !tagCommitQuery.data.exists && (
                              <Alert variant="destructive">
                                <AlertTriangle className="h-4 w-4" />
                                <AlertDescription>
                                  Tag “{normalizedTag}” exists, but its commit
                                  could not be found.
                                </AlertDescription>
                              </Alert>
                            )}

                          {tagIsVerified && tagCommitQuery.data?.commit && (
                            <CommitDetailsCard
                              commit={tagCommitQuery.data.commit}
                              label="Tag found"
                              reference={normalizedTag}
                            />
                          )}
                        </>
                      )}
                    </div>
                  </TabsContent>
                </Tabs>
              </div>

              <div className="space-y-2">
                <Label htmlFor="environment">Environment</Label>
                <Select
                  value={effectiveEnvironment?.toString() || ''}
                  onValueChange={handleEnvironmentChange}
                  disabled={environmentsQuery.isLoading}
                >
                  <SelectTrigger>
                    <SelectValue
                      placeholder={
                        environmentsQuery.isLoading
                          ? 'Loading...'
                          : 'Select environment'
                      }
                    />
                  </SelectTrigger>
                  <SelectContent>
                    {environmentsQuery.data?.map((env: EnvironmentResponse) => (
                      <SelectItem key={env.id} value={env.id.toString()}>
                        {env.name || env.slug}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <DialogFooter>
              <Button variant="outline" onClick={onClose}>
                Cancel
              </Button>
              <Button
                onClick={handleConfirm}
                disabled={
                  isLoading ||
                  !effectiveEnvironment ||
                  environmentsQuery.isLoading ||
                  (deploymentType === 'commit' &&
                    (!commitFormatIsValid ||
                      (shouldLookUpGitReference && !commitIsVerified))) ||
                  (deploymentType === 'tag' &&
                    (!tagHasValue ||
                      (shouldLookUpGitReference && !tagIsVerified)))
                }
              >
                {isLoading ? 'Deploying...' : 'Deploy'}
              </Button>
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}

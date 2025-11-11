import {
  getEnvironmentsOptions,
  getProjectBySlugOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { EnvironmentResponse, ProjectResponse } from '@/api/client/types.gen'
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
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState, useEffect } from 'react'
import { toast } from 'sonner'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { AlertTriangle } from 'lucide-react'
import { BranchSelector } from './BranchSelector'

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
}: RedeploymentModalProps) {
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
  const [branchNotFound, setBranchNotFound] = useState(false)

  // Derive effective values (either user-selected or initial/default)
  const effectiveBranch = selectedBranch || initialBranch
  const effectiveEnvironment = selectedEnvironment ?? initialEnvironment

  // When environment selection changes, automatically select its branch
  useEffect(() => {
    if (!selectedEnvironment || !environmentsQuery.data) return

    const selectedEnv = environmentsQuery.data.find(
      (env: EnvironmentResponse) => env.id === selectedEnvironment
    )

    if (!selectedEnv?.branch) return

    // Set branch type and switch to branch deployment mode
    setDeploymentType('branch')
    setSelectedBranch(selectedEnv.branch)
    setBranchNotFound(false)

    // Check if branch exists in available branches
    if (availableBranches.length > 0 && !availableBranches.includes(selectedEnv.branch)) {
      setBranchNotFound(true)
    }
  }, [selectedEnvironment, environmentsQuery.data, availableBranches])

  const validateCommit = (commit: string) => {
    const commitRegex = /^[0-9a-f]{7,40}$/
    if (!commit.trim()) return true // Optional
    if (!commitRegex.test(commit)) {
      return false
    }
    return true
  }

  const handleConfirm = async () => {
    if (deploymentType === 'commit' && !validateCommit(selectedCommit)) {
      toast.error('Invalid commit hash')
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
      commit: deploymentType === 'commit' ? selectedCommit : undefined,
      tag: deploymentType === 'tag' ? selectedTag : undefined,
      environmentId: effectiveEnvironment,
    })
  }

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Deploy Project</DialogTitle>
        </DialogHeader>
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
                      The branch "{selectedBranch}" for this environment was not found in the repository.
                      You can continue with the current branch name, or switch to deploy by commit hash.
                    </AlertDescription>
                  </Alert>
                )}
                {deploymentType === 'branch' && selectedEnvironment && !availableBranches.includes(selectedBranch) && availableBranches.length > 0 ? (
                  <div className="space-y-2">
                    <Input
                      value={selectedBranch}
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
                      projectQuery.data?.git_provider_connection_id || 0
                    }
                    defaultBranch={projectQuery.data?.main_branch}
                    value={selectedBranch}
                    onChange={(branch) => {
                      setSelectedBranch(branch)
                      setBranchNotFound(false)
                    }}
                    onBranchesLoaded={(branches) => setAvailableBranches(branches)}
                    disabled={isLoading}
                  />
                )}
              </TabsContent>
              <TabsContent value="commit">
                <Input
                  value={selectedCommit}
                  onChange={(e) => setSelectedCommit(e.target.value)}
                  placeholder="Enter commit hash"
                />
              </TabsContent>
              <TabsContent value="tag">
                <Input
                  value={selectedTag}
                  onChange={(e) => setSelectedTag(e.target.value)}
                  placeholder="Enter tag name"
                />
              </TabsContent>
            </Tabs>
          </div>

          <div className="space-y-2">
            <Label htmlFor="environment">Environment</Label>
            <Select
              value={effectiveEnvironment?.toString() || ''}
              onValueChange={(value) =>
                setSelectedEnvironment(value ? parseInt(value) : null)
              }
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
              isLoading || !effectiveEnvironment || environmentsQuery.isLoading
            }
          >
            {isLoading ? 'Deploying...' : 'Deploy'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

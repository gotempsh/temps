import { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { updateEnvironmentSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { useMutation } from '@tanstack/react-query'
import { GitBranch, Loader2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

interface EnvironmentGitConfigCardProps {
  project: ProjectResponse
  environment: EnvironmentResponse
  onUpdate: () => void
}

export function EnvironmentGitConfigCard({
  project,
  environment,
  onUpdate,
}: EnvironmentGitConfigCardProps) {
  const [branch, setBranch] = useState(environment.branch ?? '')
  const [isEditing, setIsEditing] = useState(false)

  // Sync local state when environment prop changes (e.g., after refresh)
  useEffect(() => {
    setBranch(environment.branch ?? '')
  }, [environment.branch])

  const updateEnvironmentSettings = useMutation({
    ...updateEnvironmentSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update git configuration',
    },
    onSuccess: (data) => {
      toast.success('Git configuration updated successfully.')
      setIsEditing(false)
      // Update the local state with the new branch value from the response
      setBranch(data.branch ?? '')
      onUpdate()
    },
  })

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    updateEnvironmentSettings.mutateAsync({
      path: {
        project_id: project.id,
        env_id: environment.id,
      },
      body: {
        branch: branch.trim() !== '' ? branch : null,
      },
    })
  }

  const handleCancel = () => {
    setBranch(environment.branch ?? '')
    setIsEditing(false)
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <GitBranch className="h-5 w-5" />
          Git Configuration
        </CardTitle>
        <CardDescription>
          Configure the Git branch to deploy from for this environment
        </CardDescription>
      </CardHeader>
      <CardContent>
        {isEditing ? (
          <form onSubmit={handleSubmit} className="space-y-4">
            <div>
              <Label>Branch Name</Label>
              <div className="mt-2">
                <BranchSelector
                  repoOwner={project.repo_owner || ''}
                  repoName={project.repo_name || ''}
                  connectionId={project.git_provider_connection_id || 0}
                  defaultBranch={project.main_branch}
                  value={branch}
                  onChange={setBranch}
                />
              </div>
              <p className="text-xs text-muted-foreground mt-2">
                Deployments will be triggered from this branch
              </p>
            </div>

            <div className="flex gap-2">
              <Button
                type="submit"
                disabled={updateEnvironmentSettings.isPending}
              >
                {updateEnvironmentSettings.isPending && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Save
              </Button>
              <Button
                type="button"
                variant="outline"
                onClick={handleCancel}
                disabled={updateEnvironmentSettings.isPending}
              >
                Cancel
              </Button>
            </div>
          </form>
        ) : (
          <div className="space-y-4">
            <div>
              <Label>Branch Name</Label>
              <p className="text-sm font-mono mt-1">
                {environment.branch || (
                  <span className="text-muted-foreground">Not configured</span>
                )}
              </p>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setIsEditing(true)}
            >
              Edit Branch
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

import React, { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Key,
  ExternalLink,
  CheckCircle2,
  AlertCircle,
  Loader2,
  ArrowLeft,
  Check,
} from 'lucide-react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { createGithubPatProviderMutation } from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'

interface GitHubPATFormProps {
  domain: string
  onBack?: () => void
  onSuccess?: () => void
}

export const GitHubPATForm: React.FC<GitHubPATFormProps> = ({
  domain,
  onBack,
  onSuccess,
}) => {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [token, setToken] = useState('')
  const [isSuccess, setIsSuccess] = useState(false)

  const createPATProvider = useMutation({
    ...createGithubPatProviderMutation(),
    meta: {
      errorTitle: 'Failed to configure GitHub PAT',
    },
    onSuccess: async (data) => {
      setIsSuccess(true)
      toast.success('GitHub PAT configured successfully!')

      // Invalidate git providers query to refresh the list
      await queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })

      // Wait a moment to show success state
      setTimeout(() => {
        if (onSuccess) {
          onSuccess()
        } else {
          // Navigate to dashboard or reload
          window.location.href = '/dashboard'
        }
      }, 1500)
    },
    onError: (error: any) => {
      const errorMessage =
        error?.response?.data?.detail ||
        error?.message ||
        'Failed to configure GitHub PAT'
      toast.error(errorMessage)
      console.error('PAT configuration error:', error)
    },
  })

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    // Validate token format
    if (!token) {
      toast.error('Please enter your personal access token')
      return
    }

    if (!token.startsWith('ghp_') && !token.startsWith('github_pat_')) {
      toast.warning(
        'Token should start with "ghp_" or "github_pat_". Make sure you\'re using a valid GitHub PAT.'
      )
    }

    try {
      await createPATProvider.mutateAsync({
        body: {
          name: `GitHub PAT - ${domain}`,
          token: token.trim(),
        },
      })
    } catch (error) {
      // Error is handled by onError callback
      console.error('Submit error:', error)
    }
  }

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <div className="text-center">
        <h2 className="text-2xl font-bold mb-2">
          Configure GitHub Personal Access Token
        </h2>
        <p className="text-muted-foreground">
          Connect to {domain} using a personal access token
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Setup Instructions</CardTitle>
          <CardDescription>
            Follow these steps to create and configure your GitHub PAT
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-3">
            <div className="flex items-start gap-3">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary font-semibold text-sm">
                1
              </div>
              <div className="space-y-1">
                <p className="font-medium">Generate a new token</p>
                <p className="text-sm text-muted-foreground">
                  Go to GitHub Settings → Developer settings → Personal access
                  tokens → Tokens (classic)
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    window.open(
                      `https://${domain}/settings/tokens/new`,
                      '_blank'
                    )
                  }
                >
                  <ExternalLink className="mr-2 h-3 w-3" />
                  Open GitHub Token Settings
                </Button>
              </div>
            </div>

            <div className="flex items-start gap-3">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary font-semibold text-sm">
                2
              </div>
              <div className="space-y-1">
                <p className="font-medium">Select scopes</p>
                <p className="text-sm text-muted-foreground">
                  Make sure to select these permissions:
                </p>
                <ul className="mt-2 space-y-1 text-sm text-muted-foreground">
                  <li className="flex items-center gap-2">
                    <CheckCircle2 className="h-3 w-3 text-green-600 dark:text-green-500" />
                    <code className="bg-muted px-1 rounded">repo</code> - Full
                    control of private repositories
                  </li>
                  <li className="flex items-center gap-2">
                    <CheckCircle2 className="h-3 w-3 text-green-600 dark:text-green-500" />
                    <code className="bg-muted px-1 rounded">read:user</code> -
                    Read user profile data
                  </li>
                  <li className="flex items-center gap-2">
                    <CheckCircle2 className="h-3 w-3 text-green-600 dark:text-green-500" />
                    <code className="bg-muted px-1 rounded">
                      admin:repo_hook
                    </code>{' '}
                    - Manage repository webhooks
                  </li>
                </ul>
              </div>
            </div>

            <div className="flex items-start gap-3">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary font-semibold text-sm">
                3
              </div>
              <div className="space-y-1">
                <p className="font-medium">Copy and paste the token</p>
                <p className="text-sm text-muted-foreground">
                  After creating the token, copy it and paste it below
                </p>
              </div>
            </div>
          </div>

          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              <strong>Important:</strong> Store your token securely. GitHub will
              only show it once.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">Enter Token Details</CardTitle>
        </CardHeader>
        <CardContent>
          {isSuccess ? (
            <div className="space-y-4 py-8">
              <div className="flex flex-col items-center justify-center space-y-4">
                <div className="rounded-full bg-green-100 dark:bg-green-900/20 p-3">
                  <Check className="h-8 w-8 text-green-600 dark:text-green-500" />
                </div>
                <div className="text-center space-y-2">
                  <h3 className="font-semibold text-lg">
                    Successfully Connected!
                  </h3>
                  <p className="text-sm text-muted-foreground">
                    Your GitHub account has been connected via Personal Access
                    Token.
                  </p>
                </div>
                <div className="flex items-center space-x-2 text-sm text-muted-foreground">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  <span>Redirecting to dashboard...</span>
                </div>
              </div>
            </div>
          ) : (
            <form onSubmit={handleSubmit} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="token">Personal Access Token</Label>
                <div className="relative">
                  <Input
                    id="token"
                    type="password"
                    value={token}
                    onChange={(e) => setToken(e.target.value)}
                    placeholder="ghp_xxxxxxxxxxxxxxxxxxxx"
                    required
                    autoFocus
                    disabled={createPATProvider.isPending}
                    className="pr-10"
                  />
                  {token && token.length > 10 && (
                    <div className="absolute right-3 top-1/2 -translate-y-1/2">
                      {token.startsWith('ghp_') ||
                      token.startsWith('github_pat_') ? (
                        <CheckCircle2 className="h-4 w-4 text-green-600 dark:text-green-500" />
                      ) : (
                        <AlertCircle className="h-4 w-4 text-orange-600 dark:text-orange-500" />
                      )}
                    </div>
                  )}
                </div>
                <p className="text-xs text-muted-foreground">
                  Your GitHub personal access token (starts with "ghp_" or
                  "github_pat_")
                </p>
              </div>

              <div className="flex gap-3">
                <Button
                  type="submit"
                  className="flex-1"
                  disabled={createPATProvider.isPending || !token}
                >
                  {createPATProvider.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Configuring...
                    </>
                  ) : (
                    <>
                      <Key className="mr-2 h-4 w-4" />
                      Configure PAT
                    </>
                  )}
                </Button>
                {onBack && !createPATProvider.isPending && (
                  <Button type="button" variant="outline" onClick={onBack}>
                    <ArrowLeft className="mr-2 h-4 w-4" />
                    Back
                  </Button>
                )}
              </div>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

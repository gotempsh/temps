import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { ArrowRight, ExternalLink, Loader2 } from 'lucide-react'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import {
  createGithubPatProviderMutation,
  createGitlabPatProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { listConnections } from '@/api/client'
import type { ProviderResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs'

type ProviderKind = 'github' | 'gitlab'

const DEFAULT_GITLAB_BASE_URL = 'https://gitlab.com'

/**
 * After a PAT provider is created the server provisions its connection
 * asynchronously (then syncs repos in the background). Poll `listConnections`
 * briefly for the connection belonging to the new provider so we can deep-link
 * straight to its repository list. Returns the connection id, or null if it
 * hasn't appeared within the budget — in which case we still send the user to
 * repo browsing, just without a pre-selected connection.
 */
async function resolveConnectionId(providerId: number): Promise<number | null> {
  for (let attempt = 0; attempt < 8; attempt++) {
    try {
      const { data } = await listConnections({ throwOnError: true })
      const match = data?.connections?.find((c) => c.provider_id === providerId)
      if (match) return match.id
    } catch {
      // Transient failure — keep polling within the budget.
    }
    await new Promise((resolve) => setTimeout(resolve, 400))
  }
  return null
}

// Token-creation links + the scopes Temps needs, mirrored from GitProviderFlow
// so the inline happy path stays consistent with the full setup screen.
const GITHUB_TOKEN_URL =
  'https://github.com/settings/tokens/new?description=Temps%20Platform&scopes=repo,admin:repo_hook,read:user,read:org'
const GITLAB_TOKEN_PATH = '/-/profile/personal_access_tokens'

/**
 * Inline Git-provider connect for the first-run empty state. Lets a user paste
 * a GitHub or GitLab personal access token and connect a provider without
 * leaving the project list — the happy path. On success the `listGitProviders`
 * query is invalidated, so the parent empty state re-renders with the provider
 * connected and flips its CTA to "Import a repository".
 *
 * Advanced setups (OAuth, GitHub App, self-hosted GitLab app) still live on the
 * full `/git-providers/add` screen, linked below the form.
 */
export function InlineGitConnect() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [kind, setKind] = useState<ProviderKind>('github')
  const [token, setToken] = useState('')
  const [gitlabBaseUrl, setGitlabBaseUrl] = useState(DEFAULT_GITLAB_BASE_URL)
  // True while we resolve the new connection and route onward — keeps the
  // submit button in a loading state across the mutation AND the redirect so
  // the card never flickers back to an idle form before navigating away.
  const [isRouting, setIsRouting] = useState(false)

  const onConnected = async (label: string, provider: ProviderResponse) => {
    toast.success(`${label} connected`)
    setToken('')
    setIsRouting(true)

    // Refresh the lists that drive the empty state + repo discovery.
    await queryClient.invalidateQueries({ queryKey: ['listGitProviders'] })
    await queryClient.invalidateQueries({ queryKey: ['listConnections'] })

    // Continue the happy path: go straight to choosing a repo from the
    // connection we just created (skips the import wizard's source picker). If
    // the connection isn't ready yet, land on repo browsing without a
    // pre-selected connection rather than blocking.
    const connectionId = await resolveConnectionId(provider.id)
    const target = connectionId
      ? `/projects/new?source=browse&connection=${connectionId}`
      : '/projects/new?source=browse'
    navigate(target)
  }

  const createGithub = useMutation({
    ...createGithubPatProviderMutation(),
    meta: { errorTitle: 'Failed to connect GitHub' },
    onSuccess: (data) => onConnected('GitHub', data),
  })

  const createGitlab = useMutation({
    ...createGitlabPatProviderMutation(),
    meta: { errorTitle: 'Failed to connect GitLab' },
    onSuccess: (data) => onConnected('GitLab', data),
  })

  const isSubmitting =
    createGithub.isPending || createGitlab.isPending || isRouting

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    const trimmed = token.trim()
    if (!trimmed) {
      toast.error('Please paste a personal access token')
      return
    }

    if (kind === 'github') {
      createGithub.mutate({ body: { name: 'GitHub', token: trimmed } })
    } else {
      const base = gitlabBaseUrl.trim() || DEFAULT_GITLAB_BASE_URL
      createGitlab.mutate({
        body: { name: 'GitLab', token: trimmed, base_url: base },
      })
    }
  }

  const gitlabTokenUrl = `${(gitlabBaseUrl.trim() || DEFAULT_GITLAB_BASE_URL).replace(/\/+$/, '')}${GITLAB_TOKEN_PATH}`
  const tokenHelpUrl = kind === 'github' ? GITHUB_TOKEN_URL : gitlabTokenUrl
  const tokenPlaceholder = kind === 'github' ? 'ghp_…' : 'glpat-…'

  return (
    <form onSubmit={handleSubmit} className="mt-4 flex flex-1 flex-col">
      <Tabs value={kind} onValueChange={(v) => setKind(v as ProviderKind)}>
        <TabsList className="grid w-full grid-cols-2">
          <TabsTrigger value="github" className="gap-1.5">
            <GithubIcon className="h-4 w-4" />
            GitHub
          </TabsTrigger>
          <TabsTrigger value="gitlab" className="gap-1.5">
            {/* GitlabIcon (@/icons/Gitlab) bakes in the GitLab brand accent
                (tangerine #FC6D26) itself, so no extra color className needed. */}
            <GitlabIcon className="h-4 w-4" />
            GitLab
          </TabsTrigger>
        </TabsList>

        {/* GitLab self-hosted base URL — only relevant for GitLab. Rendered
            inside the gitlab tab so it doesn't clutter the GitHub path. */}
        <TabsContent value="gitlab" className="mt-3">
          <Label htmlFor="gitlab-base-url" className="text-xs">
            GitLab URL
          </Label>
          <Input
            id="gitlab-base-url"
            value={gitlabBaseUrl}
            onChange={(e) => setGitlabBaseUrl(e.target.value)}
            placeholder={DEFAULT_GITLAB_BASE_URL}
            className="mt-1 h-9"
            autoComplete="off"
            spellCheck={false}
          />
        </TabsContent>
        {/* No GitHub-specific fields; the token input below is shared. */}
        <TabsContent value="github" className="mt-0" />
      </Tabs>

      <div className="mt-3">
        <div className="flex items-center justify-between">
          <Label htmlFor="git-token" className="text-xs">
            Personal access token
          </Label>
          <a
            href={tokenHelpUrl}
            target="_blank"
            rel="noreferrer"
            className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            Get a token
            <ExternalLink className="h-3 w-3" />
          </a>
        </div>
        <Input
          id="git-token"
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder={tokenPlaceholder}
          className="mt-1 h-9 font-mono"
          autoComplete="off"
          spellCheck={false}
        />
      </div>

      <Button type="submit" className="mt-4 w-full" disabled={isSubmitting}>
        {isSubmitting ? (
          <>
            <Loader2 className="h-4 w-4 animate-spin" />
            Connecting…
          </>
        ) : (
          <>
            {kind === 'github' ? (
              <GithubIcon className="h-4 w-4" />
            ) : (
              <GitlabIcon className="h-4 w-4" />
            )}
            Connect {kind === 'github' ? 'GitHub' : 'GitLab'}
            <ArrowRight className="h-4 w-4" />
          </>
        )}
      </Button>

      <Button
        asChild
        variant="link"
        className="mt-2 h-auto p-0 text-xs text-muted-foreground"
      >
        <Link to="/git-providers/add">
          More options — OAuth, GitHub App, self-hosted…
        </Link>
      </Button>
    </form>
  )
}

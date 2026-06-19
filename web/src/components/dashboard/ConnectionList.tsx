import { Link } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, GitBranch, Github, Gitlab } from 'lucide-react'
import {
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

// Maps a connection's provider type to its mark. github_app shares the GitHub
// glyph; anything unknown falls back to the generic branch icon.
function ProviderIcon({
  type,
  className = 'h-5 w-5',
}: {
  type: string | undefined
  className?: string
}) {
  if (type === 'github' || type === 'github_app')
    return <Github className={className} />
  if (type === 'gitlab')
    return (
      <Gitlab className={cn(className, 'text-orange-500 dark:text-orange-400')} />
    )
  return <GitBranch className={className} />
}

/**
 * Lists every connected Git account in the "Deploy from Git" card. Each row
 * shows the provider mark + account name and an Import button that deep-links
 * to that connection's repository browser (`/projects/new?source=browse&
 * connection=<id>`), so the user picks a repo from exactly the account they
 * mean. Shown once at least one provider is connected.
 */
export function ConnectionList() {
  const { data: connectionsData, isLoading } = useQuery({
    ...listConnectionsOptions({}),
  })

  // Providers carry the type per connection, so a GitLab connection renders the
  // GitLab mark rather than GitHub's.
  const { data: providers } = useQuery({
    ...listGitProvidersOptions({}),
  })

  const providerTypeFor = (providerId: number): string | undefined =>
    providers?.find((p) => p.id === providerId)?.provider_type

  const connections = connectionsData?.connections ?? []

  if (isLoading) {
    return (
      <div className="mt-4 space-y-2">
        {Array.from({ length: 2 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full rounded-lg" />
        ))}
      </div>
    )
  }

  // A provider can be connected but its connection row not yet provisioned —
  // keep a sensible fallback CTA rather than rendering an empty card.
  if (connections.length === 0) {
    return (
      <div className="mt-4 flex flex-1 flex-col justify-end">
        <Button asChild className="w-full">
          <Link
            to="/projects/new?source=browse"
            className="flex items-center justify-center gap-2"
          >
            Pick a repository
            <ArrowRight className="h-4 w-4" />
          </Link>
        </Button>
      </div>
    )
  }

  return (
    <div className="mt-4 flex flex-1 flex-col">
      <p className="mb-2 text-xs font-medium text-muted-foreground">
        Choose an account to import from
      </p>
      <ul className="space-y-2">
        {connections.map((connection) => (
          <li key={connection.id}>
            <div className="flex items-center gap-3 rounded-lg border bg-card p-3">
              <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted">
                <ProviderIcon
                  type={providerTypeFor(connection.provider_id)}
                  className="h-4 w-4 text-foreground"
                />
              </div>
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">
                  {connection.account_name}
                </p>
                <p className="truncate text-xs text-muted-foreground">
                  {connection.account_type}
                </p>
              </div>
              <Button asChild size="sm" className="shrink-0">
                <Link
                  to={`/projects/new?source=browse&connection=${connection.id}`}
                  className="flex items-center gap-1.5"
                >
                  Import
                  <ArrowRight className="h-3.5 w-3.5" />
                </Link>
              </Button>
            </div>
          </li>
        ))}
      </ul>

      <Button
        asChild
        variant="link"
        className="mt-2 h-auto justify-start p-0 text-xs text-muted-foreground"
      >
        <Link to="/git-providers/add">Connect another account</Link>
      </Button>
    </div>
  )
}

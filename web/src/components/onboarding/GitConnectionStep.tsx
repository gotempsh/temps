import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ConnectionResponse, ProviderResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { GitProviderFlow } from '@/components/git-providers/GitProviderFlow'
import {
  CheckCircle2,
  GithubIcon,
  GitBranch,
  Plus,
  ArrowRight,
  User,
  Building2,
  RefreshCw,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { formatDistanceToNow } from 'date-fns'

interface GitConnectionStepProps {
  onSuccess: () => void
  onBack?: () => void
}

function getProviderIcon(providerType: string) {
  switch (providerType) {
    case 'github':
      return GithubIcon
    case 'gitlab':
      return GitBranch
    default:
      return GitBranch
  }
}

function ConnectionCard({
  connection,
  provider,
  isSelected,
  onSelect,
}: {
  connection: ConnectionResponse
  provider?: ProviderResponse
  isSelected: boolean
  onSelect: () => void
}) {
  const Icon = getProviderIcon(provider?.provider_type || 'github')
  const AccountIcon = connection.account_type === 'Organization' ? Building2 : User

  return (
    <Card
      className={cn(
        'cursor-pointer transition-all duration-200',
        isSelected && 'ring-2 ring-primary border-primary',
        !isSelected && 'hover:border-muted-foreground/50 hover:shadow-md'
      )}
      onClick={onSelect}
    >
      <CardHeader className="pb-3">
        <div className="flex items-start justify-between">
          <div className="flex items-center gap-3">
            <div
              className={cn(
                'p-2 rounded-lg',
                isSelected ? 'bg-primary/10' : 'bg-muted'
              )}
            >
              <Icon className="h-5 w-5" />
            </div>
            <div>
              <CardTitle className="text-base flex items-center gap-2">
                {connection.account_name}
                {connection.is_active && (
                  <Badge
                    variant="outline"
                    className="text-xs bg-green-50 text-green-700 border-green-200 dark:bg-green-900/20 dark:text-green-400 dark:border-green-800"
                  >
                    Active
                  </Badge>
                )}
              </CardTitle>
              <CardDescription className="mt-0.5">
                {provider?.name || 'Git Provider'}
              </CardDescription>
            </div>
          </div>
          {isSelected && (
            <CheckCircle2 className="h-5 w-5 text-primary flex-shrink-0" />
          )}
        </div>
      </CardHeader>
      <CardContent className="pt-0">
        <div className="flex items-center gap-4 text-sm text-muted-foreground">
          <div className="flex items-center gap-1.5">
            <AccountIcon className="h-3.5 w-3.5" />
            <span>{connection.account_type}</span>
          </div>
          {connection.last_synced_at && (
            <div className="flex items-center gap-1.5">
              <RefreshCw className="h-3.5 w-3.5" />
              <span>
                Synced{' '}
                {formatDistanceToNow(new Date(connection.last_synced_at), {
                  addSuffix: true,
                })}
              </span>
            </div>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

export function GitConnectionStep({ onSuccess, onBack }: GitConnectionStepProps) {
  const [selectedConnection, setSelectedConnection] = useState<number | null>(
    null
  )
  const [showAddNew, setShowAddNew] = useState(false)

  const { data: connectionsData, isLoading: connectionsLoading } = useQuery(
    listConnectionsOptions({})
  )
  const { data: providersData } = useQuery(listGitProvidersOptions({}))

  const connections = connectionsData?.connections || []
  const providers = providersData || []

  const getProviderForConnection = (
    connection: ConnectionResponse
  ): ProviderResponse | undefined => {
    return providers.find((p) => p.id === connection.provider_id)
  }

  const handleContinue = () => {
    if (selectedConnection) {
      onSuccess()
    }
  }

  // If no connections exist, go directly to add new flow
  if (!connectionsLoading && connections.length === 0) {
    return (
      <GitProviderFlow
        onSuccess={onSuccess}
        onCancel={onBack}
        mode="onboarding"
      />
    )
  }

  // If user clicked "Add new connection", show the flow
  if (showAddNew) {
    return (
      <GitProviderFlow
        onSuccess={onSuccess}
        onCancel={() => setShowAddNew(false)}
        mode="onboarding"
      />
    )
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-xl sm:text-2xl font-bold">
          Select Git Connection
        </h2>
        <p className="text-sm sm:text-base text-muted-foreground">
          Choose an existing connection or add a new one
        </p>
      </div>

      {connectionsLoading ? (
        <div className="grid gap-4">
          {[1, 2].map((i) => (
            <Card key={i} className="animate-pulse">
              <CardHeader className="pb-3">
                <div className="flex items-center gap-3">
                  <div className="h-9 w-9 bg-muted rounded-lg" />
                  <div className="space-y-2">
                    <div className="h-4 w-32 bg-muted rounded" />
                    <div className="h-3 w-24 bg-muted rounded" />
                  </div>
                </div>
              </CardHeader>
            </Card>
          ))}
        </div>
      ) : (
        <>
          <div className="grid gap-4">
            {connections.map((connection) => (
              <ConnectionCard
                key={connection.id}
                connection={connection}
                provider={getProviderForConnection(connection)}
                isSelected={selectedConnection === connection.id}
                onSelect={() => setSelectedConnection(connection.id)}
              />
            ))}

            {/* Add new connection card */}
            <Card
              className={cn(
                'cursor-pointer transition-all duration-200 border-dashed',
                'hover:border-muted-foreground/50 hover:shadow-md'
              )}
              onClick={() => setShowAddNew(true)}
            >
              <CardHeader className="pb-3">
                <div className="flex items-center gap-3">
                  <div className="p-2 rounded-lg bg-muted">
                    <Plus className="h-5 w-5" />
                  </div>
                  <div>
                    <CardTitle className="text-base">
                      Add New Connection
                    </CardTitle>
                    <CardDescription className="mt-0.5">
                      Connect to GitHub, GitLab, or other providers
                    </CardDescription>
                  </div>
                </div>
              </CardHeader>
            </Card>
          </div>

          <div className="flex items-center justify-between pt-4">
            {onBack && (
              <Button variant="outline" onClick={onBack}>
                Back
              </Button>
            )}
            <div className="flex-1" />
            <Button
              onClick={handleContinue}
              disabled={!selectedConnection}
              className="gap-2"
            >
              Continue
              <ArrowRight className="h-4 w-4" />
            </Button>
          </div>
        </>
      )}
    </div>
  )
}

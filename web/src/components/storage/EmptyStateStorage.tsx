import { ProviderMetadata } from '@/api/client'
import { getProvidersMetadataOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Card } from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, ArrowRight, Database } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

interface EmptyStateStorageProps {}

export default function EmptyStateStorage({}: EmptyStateStorageProps) {
  const navigate = useNavigate()
  const {
    data: providers,
    isLoading,
    isError,
  } = useQuery({
    ...getProvidersMetadataOptions(),
  })

  return (
    <div className="mx-auto max-w-5xl">
      <div className="mb-8 flex flex-col items-center text-center">
        <div className="mb-4 rounded-lg bg-muted p-3">
          <Database className="size-8" />
        </div>
        <h1 className="mb-2 text-2xl font-semibold">Create a database</h1>
        <p className="max-w-prose text-base text-muted-foreground sm:text-sm">
          Pick a provider to spin up a managed database or store you can connect
          to your team&apos;s projects.
        </p>
      </div>

      {/* Loading state — skeleton grid mirrors the real provider cards */}
      {isLoading && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {[...Array(6)].map((_, i) => (
            <Card key={i} className="flex items-start gap-4 p-5">
              <div className="size-12 shrink-0 rounded-md bg-muted animate-pulse" />
              <div className="flex-1 space-y-2 pt-1">
                <div className="h-4 w-28 rounded bg-muted animate-pulse" />
                <div className="h-3 w-full rounded bg-muted animate-pulse" />
                <div className="h-3 w-3/4 rounded bg-muted animate-pulse" />
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Error state */}
      {isError && (
        <Alert variant="destructive">
          <AlertCircle className="size-4" />
          <AlertDescription>
            Failed to load available providers. Please try again later.
          </AlertDescription>
        </Alert>
      )}

      {/* Providers grid — every option visible at a glance */}
      {providers && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {providers.map((provider: ProviderMetadata) => (
            <ProviderCard
              key={provider.service_type}
              provider={provider}
              onSelect={() =>
                navigate(`/storage/create?type=${provider.service_type}`)
              }
            />
          ))}
        </div>
      )}
    </div>
  )
}

function ProviderCard({
  provider,
  onSelect,
}: {
  provider: ProviderMetadata
  onSelect: () => void
}) {
  return (
    <Card
      role="button"
      tabIndex={0}
      onClick={onSelect}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          onSelect()
        }
      }}
      className="group relative flex cursor-pointer flex-col gap-3 p-5 hover:border-foreground/20 hover:shadow-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      <div className="flex items-center gap-3">
        <div
          className="flex size-12 shrink-0 items-center justify-center rounded-md"
          style={{ backgroundColor: provider.color }}
        >
          <img
            src={provider.icon_url}
            alt={`${provider.display_name} logo`}
            width={28}
            height={28}
            className="size-7 rounded-md brightness-0 invert"
          />
        </div>
        <h3 className="min-w-0 flex-1 truncate text-base font-semibold sm:text-sm">
          {provider.display_name}
        </h3>
        <ArrowRight className="size-4 shrink-0 text-muted-foreground/40 transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
      </div>
      <p className="line-clamp-2 text-sm text-muted-foreground">
        {provider.description}
      </p>
    </Card>
  )
}

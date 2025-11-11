import { ProviderMetadata } from '@/api/client'
import { getProvidersMetadataOptions } from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, Database, Loader2 } from 'lucide-react'
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
    <div className="mx-auto max-w-4xl">
      <div className="flex flex-col items-center text-center mb-8">
        <div className="mb-4 p-3 rounded-lg bg-muted">
          <Database className="h-8 w-8" />
        </div>
        <h1 className="text-2xl font-semibold mb-2">Create a database</h1>
        <p className="text-muted-foreground">
          Create databases and stores that you can connect to your team&apos;s
          projects.
        </p>
      </div>

      <div className="space-y-6">
        <Alert>
          <AlertCircle className="h-4 w-4" />
          <AlertDescription className="flex items-center gap-2">
            Select a database provider to get started with your application.
          </AlertDescription>
        </Alert>

        {/* Loading state */}
        {isLoading && (
          <div className="flex items-center justify-center py-12">
            <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
          </div>
        )}

        {/* Error state */}
        {isError && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              Failed to load available providers. Please try again later.
            </AlertDescription>
          </Alert>
        )}

        {/* Providers list */}
        {providers && (
          <div className="space-y-4">
            {providers.map((provider: ProviderMetadata) => (
              <Card key={provider.service_type}>
                <CardContent className="p-6">
                  <div className="flex items-start gap-4">
                    <div
                      className="flex items-center justify-center rounded-md p-2"
                      style={{ backgroundColor: provider.color }}
                    >
                      <img
                        src={provider.icon_url}
                        alt={`${provider.display_name} logo`}
                        width={32}
                        height={32}
                        className="rounded-md brightness-0 invert"
                      />
                    </div>
                    <div className="flex-1 space-y-1">
                      <div className="flex items-center justify-between">
                        <div>
                          <h3 className="font-semibold">
                            {provider.display_name}
                          </h3>
                          <p className="text-sm text-muted-foreground">
                            {provider.description}
                          </p>
                        </div>
                        <Button
                          onClick={() =>
                            navigate(
                              `/storage/create?type=${provider.service_type}`
                            )
                          }
                        >
                          Create
                        </Button>
                      </div>
                    </div>
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

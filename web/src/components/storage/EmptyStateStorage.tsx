import { ServiceTypeRoute } from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { ServiceLogo } from '@/components/ui/service-logo'
import { AlertCircle, Database } from 'lucide-react'

const providers = [
  {
    id: 'postgres',
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
  },
  {
    id: 's3',
    name: 'MinIO',
    description: 'High Performance Object Storage',
  },
  {
    id: 'redis',
    name: 'Redis',
    description: 'In-Memory Data Store',
  },
] as {
  id: ServiceTypeRoute
  name: string
  description: string
  logo: string
}[]
interface EmptyStateStorageProps {
  onCreateClick: (serviceType: ServiceTypeRoute) => void
}

export default function EmptyStateStorage({
  onCreateClick,
}: EmptyStateStorageProps) {
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

        <div className="space-y-4">
          {providers.map((provider) => (
            <Card key={provider.name}>
              <CardContent className="p-6">
                <div className="flex items-start gap-4">
                  <ServiceLogo service={provider.id} />
                  <div className="flex-1 space-y-1">
                    <div className="flex items-center justify-between">
                      <div>
                        <h3 className="font-semibold">{provider.name}</h3>
                        <p className="text-sm text-muted-foreground">
                          {provider.description}
                        </p>
                      </div>
                      <Button onClick={() => onCreateClick(provider.id)}>
                        Create
                      </Button>
                    </div>
                  </div>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      </div>
    </div>
  )
}

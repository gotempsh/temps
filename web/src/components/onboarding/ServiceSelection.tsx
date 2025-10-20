import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Badge } from '@/components/ui/badge'
import {
  ArrowRight,
  ArrowLeft,
  Database,
  Server,
  HardDrive,
  Info,
} from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'

export type ServiceType = 'postgresql' | 'redis' | 's3'

interface ServiceSelectionProps {
  onBack?: () => void
  onServicesSelect: (services: ServiceType[]) => void
}

interface ServiceOption {
  id: ServiceType
  name: string
  icon: React.ElementType
  description: string
  features: string[]
  recommended?: boolean
}

const services: ServiceOption[] = [
  {
    id: 'postgresql',
    name: 'PostgreSQL',
    icon: Database,
    description: 'Relational database for structured data',
    features: [
      'ACID compliance',
      'Complex queries with SQL',
      'Automatic backups',
      'Connection pooling',
    ],
    recommended: true,
  },
  {
    id: 'redis',
    name: 'Redis',
    icon: Server,
    description: 'In-memory data store for caching and sessions',
    features: [
      'Lightning-fast performance',
      'Session management',
      'Caching layer',
      'Pub/Sub messaging',
    ],
  },
  {
    id: 's3',
    name: 'S3 Compatible Storage',
    icon: HardDrive,
    description: 'Object storage for files and media',
    features: [
      'File uploads',
      'Media storage',
      'Static asset hosting',
      'Automatic scaling',
    ],
  },
]

export function ServiceSelection({
  onBack,
  onServicesSelect,
}: ServiceSelectionProps) {
  const [selectedServices, setSelectedServices] = useState<Set<ServiceType>>(
    new Set(['postgresql'])
  )

  const toggleService = (serviceId: ServiceType) => {
    const newServices = new Set(selectedServices)
    if (newServices.has(serviceId)) {
      newServices.delete(serviceId)
    } else {
      newServices.add(serviceId)
    }
    setSelectedServices(newServices)
  }

  const handleContinue = () => {
    onServicesSelect(Array.from(selectedServices))
  }

  const handleSkip = () => {
    onServicesSelect([])
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold">Select Services for Your Project</h2>
        <p className="text-muted-foreground mt-2">
          Choose the services your application needs. You can always add more
          later.
        </p>
      </div>

      <Alert>
        <Info className="h-4 w-4" />
        <AlertDescription>
          Services are automatically provisioned and configured for your
          project. Connection details will be available as environment
          variables.
        </AlertDescription>
      </Alert>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        {services.map((service) => {
          const Icon = service.icon
          const isSelected = selectedServices.has(service.id)

          return (
            <Card
              key={service.id}
              className={`relative cursor-pointer transition-all ${
                isSelected
                  ? 'ring-2 ring-primary border-primary'
                  : 'hover:border-muted-foreground/50'
              }`}
              onClick={() => toggleService(service.id)}
            >
              {service.recommended && (
                <Badge
                  variant="default"
                  className="absolute top-3 right-3 text-xs"
                >
                  Recommended
                </Badge>
              )}
              <CardHeader>
                <div className="flex items-start justify-between">
                  <div className="flex items-center gap-3">
                    <Icon className="h-8 w-8 text-muted-foreground" />
                    <div>
                      <CardTitle className="text-lg">{service.name}</CardTitle>
                      <CardDescription className="text-xs mt-1">
                        {service.description}
                      </CardDescription>
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-2 mt-3">
                  <Checkbox
                    checked={isSelected}
                    onCheckedChange={() => toggleService(service.id)}
                    onClick={(e) => e.stopPropagation()}
                  />
                  <label className="text-sm font-medium cursor-pointer">
                    Include {service.name}
                  </label>
                </div>
              </CardHeader>
              <CardContent>
                <ul className="space-y-1">
                  {service.features.map((feature, idx) => (
                    <li
                      key={idx}
                      className="text-sm text-muted-foreground flex items-start"
                    >
                      <span className="mr-2 text-primary">•</span>
                      {feature}
                    </li>
                  ))}
                </ul>
              </CardContent>
            </Card>
          )
        })}
      </div>

      <Card className="border-dashed bg-muted/30">
        <CardHeader>
          <CardTitle className="text-lg">No Services Needed?</CardTitle>
          <CardDescription>
            If your application doesn&apos;t require additional services, you
            can skip this step
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Button variant="outline" onClick={handleSkip} className="w-full">
            Skip Service Selection
            <ArrowRight className="ml-2 h-4 w-4" />
          </Button>
        </CardContent>
      </Card>

      <div className="flex justify-between">
        {onBack && (
          <Button variant="outline" onClick={onBack}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back
          </Button>
        )}
        {selectedServices.size > 0 && (
          <Button onClick={handleContinue} className="ml-auto">
            Continue with {selectedServices.size} service
            {selectedServices.size !== 1 ? 's' : ''}
            <ArrowRight className="ml-2 h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  )
}

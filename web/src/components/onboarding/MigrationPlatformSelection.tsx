import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  ArrowRight,
  ArrowLeft,
  Cloud,
  Server,
  Package,
  HelpCircle,
} from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'

export type MigrationPlatform = 'vercel' | 'coolify' | 'dokploy' | 'none'

interface MigrationPlatformSelectionProps {
  onBack?: () => void
  onPlatformSelect: (platform: MigrationPlatform) => void
}

interface PlatformOption {
  id: MigrationPlatform
  name: string
  icon: React.ElementType
  description: string
  features: string[]
  available: boolean
}

const platforms: PlatformOption[] = [
  {
    id: 'vercel',
    name: 'Vercel',
    icon: Cloud,
    description: 'Migrate from Vercel deployments',
    features: [
      'Automatic project detection',
      'Environment variables migration',
      'Domain configuration transfer',
      'Build settings import',
    ],
    available: false,
  },
  {
    id: 'coolify',
    name: 'Coolify',
    icon: Server,
    description: 'Migrate from Coolify self-hosted platform',
    features: [
      'Docker configuration import',
      'Service migration',
      'Database connections',
      'Custom domain mapping',
    ],
    available: false,
  },
  {
    id: 'dokploy',
    name: 'Dokploy',
    icon: Package,
    description: 'Migrate from Dokploy deployments',
    features: [
      'Application import',
      'Database migration',
      'SSL certificate transfer',
      'Resource allocation',
    ],
    available: false,
  },
]

export function MigrationPlatformSelection({
  onBack,
  onPlatformSelect,
}: MigrationPlatformSelectionProps) {
  const [selectedPlatform, setSelectedPlatform] =
    useState<MigrationPlatform | null>(null)

  const handlePlatformClick = (platform: PlatformOption) => {
    if (!platform.available) return
    setSelectedPlatform(platform.id)
  }

  const handleContinue = () => {
    if (selectedPlatform) {
      onPlatformSelect(selectedPlatform)
    }
  }

  const handleSkip = () => {
    onPlatformSelect('none')
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold">Migrate From Another Platform?</h2>
        <p className="text-muted-foreground mt-2">
          Import your existing projects and configurations from other platforms
        </p>
      </div>

      <Alert>
        <HelpCircle className="h-4 w-4" />
        <AlertDescription>
          Migration tools help you quickly move your existing deployments to
          Temps. You can also start fresh and skip this step.
        </AlertDescription>
      </Alert>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        {platforms.map((platform) => {
          const Icon = platform.icon
          return (
            <Card
              key={platform.id}
              className={`relative cursor-pointer transition-all ${
                !platform.available
                  ? 'opacity-60 cursor-not-allowed'
                  : selectedPlatform === platform.id
                    ? 'ring-2 ring-primary border-primary'
                    : 'hover:border-muted-foreground/50'
              }`}
              onClick={() => handlePlatformClick(platform)}
            >
              {!platform.available && (
                <Badge
                  variant="secondary"
                  className="absolute top-3 right-3 text-xs"
                >
                  Coming Soon
                </Badge>
              )}
              <CardHeader>
                <div className="flex items-center gap-3">
                  <Icon className="h-8 w-8 text-muted-foreground" />
                  <div>
                    <CardTitle className="text-lg">{platform.name}</CardTitle>
                    <CardDescription className="text-xs mt-1">
                      {platform.description}
                    </CardDescription>
                  </div>
                </div>
              </CardHeader>
              <CardContent>
                <ul className="space-y-1">
                  {platform.features.map((feature, idx) => (
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

      <Card className="border-dashed">
        <CardHeader>
          <CardTitle className="text-lg">Start Fresh</CardTitle>
          <CardDescription>
            Skip migration and set up your projects from scratch
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground mb-4">
            You can always import projects later or manually configure your
            deployments.
          </p>
          <Button variant="outline" onClick={handleSkip} className="w-full">
            Skip Migration
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
        {selectedPlatform && (
          <Button onClick={handleContinue} className="ml-auto">
            Continue with{' '}
            {platforms.find((p) => p.id === selectedPlatform)?.name}
            <ArrowRight className="ml-2 h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  )
}

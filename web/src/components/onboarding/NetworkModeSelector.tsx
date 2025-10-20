import { Button } from '@/components/ui/button'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Server,
  Router,
  Cloud,
  Info,
  CheckCircle2,
  AlertCircle,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { usePlatformAccess } from '@/contexts/PlatformAccessContext'

export type NetworkMode = 'direct' | 'nat' | 'cloudflare'

interface NetworkModeOption {
  id: NetworkMode
  title: string
  subtitle: string
  description: string
  icon: React.ReactNode
  recommended: boolean
  pros: string[]
  cons: string[]
  technicalDetails: string
}

interface NetworkModeSelectorProps {
  selectedMode: NetworkMode | null
  onSelect: (mode: NetworkMode) => void
  onNext: () => void
  onBack: () => void
}

export function NetworkModeSelector({
  selectedMode,
  onSelect,
  onNext,
  onBack,
}: NetworkModeSelectorProps) {
  const { accessInfo } = usePlatformAccess()

  const modes: NetworkModeOption[] = [
    {
      id: 'direct',
      title: 'Direct/VPS',
      subtitle: 'Public IP Address',
      description:
        'Your server has a direct public IP address (e.g., VPS, cloud VM)',
      icon: <Server className="h-6 w-6" />,
      recommended: accessInfo?.access_mode === 'direct',
      pros: [
        'Simple setup - just add DNS records',
        'Full control over networking',
        'Best performance',
        'No additional dependencies',
      ],
      cons: ['Requires public IP address', 'Direct exposure to internet'],
      technicalDetails: 'Point your DNS A record to your public IP',
    },
    {
      id: 'nat',
      title: 'NAT/Port Forwarding',
      subtitle: 'Behind Router',
      description:
        'Your server is behind a router or firewall (home network, corporate)',
      icon: <Router className="h-6 w-6" />,
      recommended: accessInfo?.access_mode === 'nat',
      pros: [
        'Works from home/office network',
        'No additional services needed',
        'Full control over ports',
        'Cost-effective',
      ],
      cons: [
        'Requires router configuration',
        'May not work with CGNAT',
        'ISP may block ports 80/443',
      ],
      technicalDetails: 'Configure port forwarding for ports 80 and 443',
    },
    {
      id: 'cloudflare',
      title: 'Cloudflare Tunnel',
      subtitle: 'No Port Forwarding',
      description:
        'Use Cloudflare Tunnel to expose your server without port forwarding',
      icon: <Cloud className="h-6 w-6" />,
      recommended: accessInfo?.access_mode === 'cloudflare_tunnel',
      pros: [
        'No port forwarding required',
        'Works anywhere (even CGNAT)',
        'DDoS protection included',
        'Secure tunnel connection',
      ],
      cons: [
        'Requires Cloudflare account',
        'Wildcard domains must be at root',
        'Adds slight latency',
        'Depends on Cloudflare service',
      ],
      technicalDetails: 'Install cloudflared and configure tunnel',
    },
  ]

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Select Network Mode</h2>
        <p className="text-muted-foreground">
          Choose how your server will be accessed from the internet
        </p>
      </div>

      {accessInfo && (
        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription>
            Based on your current setup, we recommend{' '}
            <strong>
              {modes.find((m) => m.recommended)?.title || 'Direct/VPS'}
            </strong>{' '}
            mode.
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-3">
        {modes.map((mode) => (
          <button
            key={mode.id}
            onClick={() => onSelect(mode.id)}
            className={cn(
              'relative p-5 border-2 rounded-lg transition-all text-left group hover:border-primary/50 w-full',
              selectedMode === mode.id
                ? 'border-primary bg-primary/5'
                : 'border-border hover:bg-accent/30'
            )}
          >
            <div className="flex items-start gap-4">
              <div
                className={cn(
                  'flex h-12 w-12 items-center justify-center rounded-lg transition-colors flex-shrink-0',
                  selectedMode === mode.id
                    ? 'bg-primary text-primary-foreground'
                    : 'bg-muted text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary'
                )}
              >
                {mode.icon}
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2 mb-1 flex-wrap">
                  <h3 className="font-semibold text-base">{mode.title}</h3>
                  <span className="text-xs text-muted-foreground">
                    {mode.subtitle}
                  </span>
                  {mode.recommended && (
                    <span className="text-xs font-medium px-2 py-0.5 rounded-full bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400">
                      Recommended
                    </span>
                  )}
                </div>
                <p className="text-sm text-muted-foreground mb-3">
                  {mode.description}
                </p>

                {/* Pros and Cons */}
                <div className="grid grid-cols-1 md:grid-cols-2 gap-3 mb-3">
                  <div className="space-y-1">
                    <p className="text-xs font-medium text-green-600 dark:text-green-400">
                      Pros:
                    </p>
                    <ul className="space-y-0.5">
                      {mode.pros.map((pro, i) => (
                        <li
                          key={i}
                          className="flex items-start gap-1.5 text-xs text-muted-foreground"
                        >
                          <CheckCircle2 className="h-3 w-3 text-green-600 dark:text-green-400 flex-shrink-0 mt-0.5" />
                          <span>{pro}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                  <div className="space-y-1">
                    <p className="text-xs font-medium text-orange-600 dark:text-orange-400">
                      Cons:
                    </p>
                    <ul className="space-y-0.5">
                      {mode.cons.map((con, i) => (
                        <li
                          key={i}
                          className="flex items-start gap-1.5 text-xs text-muted-foreground"
                        >
                          <AlertCircle className="h-3 w-3 text-orange-600 dark:text-orange-400 flex-shrink-0 mt-0.5" />
                          <span>{con}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                </div>

                {/* Technical Details */}
                <div className="text-xs text-muted-foreground bg-muted/50 rounded px-2 py-1.5">
                  <strong>Setup:</strong> {mode.technicalDetails}
                </div>
              </div>
              {selectedMode === mode.id && (
                <CheckCircle2 className="h-5 w-5 text-primary absolute top-4 right-4" />
              )}
            </div>
          </button>
        ))}
      </div>

      <div className="flex justify-between pt-4">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={onNext} disabled={!selectedMode}>
          Continue
        </Button>
      </div>
    </div>
  )
}

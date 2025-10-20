import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Globe, Server, Info } from 'lucide-react'
import { useQuery } from '@tanstack/react-query'
import { getPublicIpOptions } from '@/api/client/@tanstack/react-query.gen'
import { usePlatformAccess } from '@/contexts/PlatformAccessContext'

interface BaseDomainStepProps {
  value: string
  onChange: (value: string) => void
  onNext: () => void
  onBack: () => void
}

export function BaseDomainStep({
  value,
  onChange,
  onNext,
  onBack,
}: BaseDomainStepProps) {
  const [error, setError] = useState<string | null>(null)
  const { accessInfo } = usePlatformAccess()

  // Get public IP
  const { data: publicIpData, isLoading: ipLoading } = useQuery({
    ...getPublicIpOptions(),
    retry: 2,
  })

  // Validate domain format
  const validateDomain = (domain: string): boolean => {
    if (!domain) {
      setError('Domain is required')
      return false
    }

    // Basic domain validation
    const domainRegex =
      /^(?:[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$/
    if (!domainRegex.test(domain)) {
      setError('Invalid domain format. Example: example.com')
      return false
    }

    setError(null)
    return true
  }

  const handleNext = () => {
    if (validateDomain(value)) {
      onNext()
    }
  }

  const handleChange = (newValue: string) => {
    onChange(newValue)
    if (error) {
      setError(null)
    }
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Configure Base Domain</h2>
        <p className="text-muted-foreground">
          This domain will be used for all your deployments
        </p>
      </div>

      {/* Public IP Display */}
      {accessInfo?.public_ip && (
        <Alert>
          <Server className="h-4 w-4" />
          <AlertDescription>
            <div className="flex items-center justify-between">
              <span className="text-sm">
                Your public IP:{' '}
                <code className="font-mono text-xs bg-muted px-1.5 py-0.5 rounded">
                  {accessInfo.public_ip}
                </code>
              </span>
            </div>
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="baseDomain">Base Domain</Label>
          <div className="relative">
            <Globe className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <Input
              id="baseDomain"
              value={value}
              onChange={(e) => handleChange(e.target.value)}
              placeholder="example.com"
              className="pl-10"
              autoFocus
            />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <p className="text-xs text-muted-foreground">
            Enter your domain without "www" or subdomains (e.g., example.com)
          </p>
        </div>

        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription>
            <div className="space-y-2 text-sm">
              <p className="font-medium">This domain will be used to create:</p>
              <ul className="space-y-1 ml-4">
                <li>
                  • Wildcard domain for all projects: *.{value || 'example.com'}
                </li>
                <li>
                  • Preview deployments: *.preview.{value || 'example.com'}
                </li>
                <li>• Main access URL: temps.{value || 'example.com'}</li>
              </ul>
            </div>
          </AlertDescription>
        </Alert>

        <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
          <Info className="h-4 w-4 text-blue-600" />
          <AlertDescription className="text-sm">
            <strong>Next step:</strong> You'll need to configure DNS records to
            point this domain to your server.
          </AlertDescription>
        </Alert>
      </div>

      <div className="flex justify-between pt-4">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={handleNext} disabled={!value || !!error}>
          Continue
        </Button>
      </div>
    </div>
  )
}

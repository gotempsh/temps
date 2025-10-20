import { useState, useEffect, useMemo } from 'react'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { CodeBlock, InlineCode } from '@/components/ui/code-block'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  CheckCircle2,
  Circle,
  Globe,
  Server,
  Network,
  Loader2,
  AlertCircle,
  Shield,
  Info,
  Copy,
  Router,
  Cloud,
  RefreshCw,
  Settings,
  ChevronDown,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  getPublicIpOptions,
  createDomainMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useSettings } from '@/hooks/useSettings'
import { toast } from 'sonner'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useNavigate } from 'react-router-dom'
import { usePlatformAccess } from '@/contexts/PlatformAccessContext'

type SetupStep = 'detection' | 'domain' | 'complete'
type NetworkSolution = 'direct' | 'port-forwarding' | 'cloudflare-tunnel'

interface ExternalConnectivitySetupProps {
  onComplete?: () => void
}

interface SetupOption {
  id: NetworkSolution
  title: string
  description: string
  icon: React.ReactNode
  recommended: boolean
  pros: string[]
  cons: string[]
}

export function ExternalConnectivitySetup({
  onComplete,
}: ExternalConnectivitySetupProps) {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: _settings } = useSettings()
  const navigate = useNavigate()

  // Use platform access context to detect actual access mode
  const {
    accessInfo,
    isLoading: accessLoading,
    isDirect,
    isNat,
    isLocal,
  } = usePlatformAccess()

  // Check if external connectivity is already configured
  const isExternallyConfigured = () => {
    const isHttps = window.location.protocol === 'https:'
    const hostname = window.location.hostname
    const isNotLocalhost =
      hostname !== 'localhost' &&
      hostname !== '127.0.0.1' &&
      !hostname.startsWith('192.168.') &&
      !hostname.startsWith('10.') &&
      !hostname.startsWith('172.')
    return isHttps && isNotLocalhost
  }

  // Check if accessing from private IP (behind router)
  const isPrivateIP = () => {
    const hostname = window.location.hostname
    return (
      hostname.startsWith('192.168.') ||
      hostname.startsWith('10.') ||
      (hostname.startsWith('172.') &&
        parseInt(hostname.split('.')[1]) >= 16 &&
        parseInt(hostname.split('.')[1]) <= 31)
    )
  }

  // Get current access port
  const getCurrentPort = () => {
    const port = window.location.port
    if (port) return port
    return window.location.protocol === 'https:' ? '443' : '80'
  }

  const [isConfigured, setIsConfigured] = useState(isExternallyConfigured)
  const [showPortMapping] = useState(isPrivateIP)

  // Helper function to slugify domain name for tunnel
  const slugifyDomain = (domain: string) => {
    return domain
      .toLowerCase()
      .replace(/\./g, '-')
      .replace(/[^a-z0-9-]/g, '')
      .replace(/-+/g, '-')
      .replace(/^-|-$/g, '')
  }

  // Setup steps
  const [currentStep, setCurrentStep] = useState<SetupStep>('detection')
  const [networkSolution, setNetworkSolution] =
    useState<NetworkSolution | null>(null)
  const [domainName, setDomainName] = useState('*.example.com')
  const [_showAdvanced, _setShowAdvanced] = useState(false)
  const [setupInstructionsOpen, setSetupInstructionsOpen] = useState(true)
  const [confirmedPublicAccess, setConfirmedPublicAccess] = useState(false)

  // Cloudflare tunnel setup
  const [baseDomain, setBaseDomain] = useState('example.com')

  // Generate tunnel name based on domain
  const tunnelName = `${slugifyDomain(baseDomain)}-temps`

  // Port forwarding setup
  const [_forwardedPorts, _setForwardedPorts] = useState(['80', '443'])
  const [_routerConfigured, _setRouterConfigured] = useState(false)

  // Public IP detection - prefer platform access info if available
  const {
    data: publicIpData,
    isLoading: ipLoading,
    isFetching: ipFetching,
    refetch: refetchIp,
  } = useQuery({
    ...getPublicIpOptions(),
    enabled: !accessInfo?.public_ip,
  })

  // Use platform access public IP if available
  const effectivePublicIP = useMemo(
    () =>
      accessInfo?.public_ip ||
      (typeof publicIpData === 'string'
        ? publicIpData
        : publicIpData &&
            typeof publicIpData === 'object' &&
            'ip' in publicIpData
          ? publicIpData.ip
          : null),
    [accessInfo?.public_ip, publicIpData]
  ) as string | null

  // Use isFetching to show loading state during refetch as well
  const isLoadingIp = ipLoading || ipFetching

  // Get recommended setup options based on detected environment
  const getSetupOptions = (): SetupOption[] => {
    const options: SetupOption[] = []

    // If direct access (public VPS), recommend direct setup
    if (isDirect || accessInfo?.public_ip) {
      options.push({
        id: 'direct',
        title: 'Direct Setup',
        description: 'Your server has a public IP and can be accessed directly',
        icon: <Server className="h-5 w-5 text-blue-600" />,
        recommended: true,
        pros: [
          'Fastest performance',
          'No extra configuration needed',
          'Simple DNS setup',
        ],
        cons: ['Requires public IP address'],
      })
    }

    // If behind NAT or local, offer both port forwarding and Cloudflare tunnel
    if (isNat || isLocal || (!isDirect && !accessInfo?.public_ip)) {
      options.push({
        id: 'cloudflare-tunnel',
        title: 'Cloudflare Tunnel',
        description:
          'Secure tunnel without exposing ports or needing a public IP',
        icon: <Cloud className="h-5 w-5 text-orange-600" />,
        recommended: true,
        pros: [
          'No port forwarding needed',
          'Works anywhere',
          'DDoS protection',
        ],
        cons: ['Requires Cloudflare account', 'Extra service to manage'],
      })

      options.push({
        id: 'port-forwarding',
        title: 'Port Forwarding',
        description: 'Configure your router to forward ports to this server',
        icon: <Router className="h-5 w-5 text-green-600" />,
        recommended: false,
        pros: ['Direct connection', 'No third-party service'],
        cons: ['Requires router access', 'More complex setup'],
      })
    }

    return options
  }

  const setupOptions = getSetupOptions()

  useEffect(() => {
    setBreadcrumbs([{ label: 'External Connectivity Setup' }])
  }, [setBreadcrumbs])

  usePageTitle('External Connectivity Setup')

  // Create domain mutation
  const createDomain = useMutation({
    ...createDomainMutation({}),
    meta: {
      errorTitle: 'Failed to configure domain',
    },
    onSuccess: (data) => {
      toast.success(
        'Domain configured successfully! Redirecting to DNS challenge setup...'
      )

      // Always redirect to domain detail page after creating domain
      if (data && data.id) {
        navigate(`/domains/${data.id}`)
      } else {
        console.error('Domain created but no ID returned:', data)
        toast.error('Domain created but could not navigate to detail page')
      }
    },
    onError: (error: any) => {
      console.error('Failed to create domain:', error)
      toast.error(error?.detail || 'Failed to configure domain')
    },
  })

  const handleSolutionSelect = (solution: NetworkSolution) => {
    setNetworkSolution(solution)
    setCurrentStep('domain')
  }

  const handleDomainSubmit = async () => {
    if (domainName && domainName.startsWith('*.')) {
      await createDomain.mutateAsync({
        body: {
          domain: domainName,
          challenge_type: 'dns-01', // Always use DNS challenge for all domain configurations
        },
      })
    }
  }

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text)
    toast.success('Copied to clipboard!')
  }

  const isStepComplete = (step: SetupStep): boolean => {
    switch (step) {
      case 'detection':
        return networkSolution !== null
      case 'domain':
        return false
      case 'complete':
        return true
      default:
        return false
    }
  }

  const getCurrentStepNumber = (): number => {
    switch (currentStep) {
      case 'detection':
        return 1
      case 'domain':
        return 2
      case 'complete':
        return 3
      default:
        return 1
    }
  }

  // Show configured state if already accessible via HTTPS with proper domain
  if (isConfigured) {
    return (
      <div className="max-w-4xl mx-auto space-y-6">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">
            External Connectivity
          </h1>
          <p className="text-muted-foreground mt-2">
            Your platform&apos;s external connectivity status and configuration
          </p>
        </div>

        <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <CheckCircle2 className="h-6 w-6 text-green-600" />
              External Connectivity Active
            </CardTitle>
            <CardDescription>
              Your platform is accessible from the internet via HTTPS
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <h4 className="font-medium flex items-center gap-2">
                  <Shield className="h-4 w-4 text-green-600" />
                  Security Status
                </h4>
                <div className="flex items-center gap-2">
                  <Badge
                    variant="secondary"
                    className="bg-green-100 text-green-800"
                  >
                    HTTPS Enabled
                  </Badge>
                  <Badge
                    variant="secondary"
                    className="bg-green-100 text-green-800"
                  >
                    SSL Certificate Active
                  </Badge>
                </div>
              </div>

              <div className="space-y-2">
                <h4 className="font-medium flex items-center gap-2">
                  <Globe className="h-4 w-4 text-blue-600" />
                  Access Information
                </h4>
                <div className="space-y-1">
                  <div className="flex items-center gap-2 text-sm">
                    <span className="text-muted-foreground">Current URL:</span>
                    <code className="bg-muted px-2 py-1 rounded text-xs">
                      {window.location.origin}
                    </code>
                  </div>
                  <div className="flex items-center gap-2 text-sm">
                    <span className="text-muted-foreground">Domain:</span>
                    <code className="bg-muted px-2 py-1 rounded text-xs">
                      {window.location.hostname}
                    </code>
                  </div>
                </div>
              </div>
            </div>

            <Alert>
              <Info className="h-4 w-4" />
              <AlertDescription>
                Your platform is currently accessible from the internet. New
                deployments will automatically be available at subdomains based
                on your current domain configuration.
              </AlertDescription>
            </Alert>

            {/* Port Mapping Instructions for Private IP */}
            {showPortMapping && (
              <Card className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/10">
                <CardHeader>
                  <CardTitle className="flex items-center gap-2 text-lg">
                    <Router className="h-5 w-5 text-orange-600" />
                    Router Port Forwarding Configuration
                  </CardTitle>
                  <CardDescription>
                    You&apos;re accessing from a private IP. Ensure these port
                    mappings are configured in your router.
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                  <div className="space-y-2">
                    <h4 className="font-medium flex items-center gap-2">
                      <Network className="h-4 w-4" />
                      Current Access Information
                    </h4>
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                      <div className="p-3 bg-white dark:bg-gray-900 rounded-md border">
                        <div className="text-xs text-muted-foreground mb-1">
                          Private IP Address
                        </div>
                        <code className="text-sm font-mono font-semibold">
                          {window.location.hostname}
                        </code>
                      </div>
                      <div className="p-3 bg-white dark:bg-gray-900 rounded-md border">
                        <div className="text-xs text-muted-foreground mb-1">
                          Current Port
                        </div>
                        <code className="text-sm font-mono font-semibold">
                          {getCurrentPort()}
                        </code>
                      </div>
                    </div>
                  </div>

                  <Separator />

                  <div className="space-y-3">
                    <h4 className="font-medium">Required Port Mappings</h4>
                    <div className="space-y-2">
                      <div className="flex items-center justify-between p-3 bg-white dark:bg-gray-900 rounded-md border">
                        <div className="flex items-center gap-3">
                          <div>
                            <div className="text-sm font-medium">
                              HTTP Traffic
                            </div>
                            <div className="text-xs text-muted-foreground">
                              For initial connections and redirects
                            </div>
                          </div>
                        </div>
                        <div className="text-right">
                          <div className="text-xs text-muted-foreground">
                            External → Internal
                          </div>
                          <code className="text-sm font-mono">
                            80 → {window.location.hostname}:80
                          </code>
                        </div>
                      </div>
                      <div className="flex items-center justify-between p-3 bg-white dark:bg-gray-900 rounded-md border">
                        <div className="flex items-center gap-3">
                          <div>
                            <div className="text-sm font-medium">
                              HTTPS Traffic
                            </div>
                            <div className="text-xs text-muted-foreground">
                              For secure connections
                            </div>
                          </div>
                        </div>
                        <div className="text-right">
                          <div className="text-xs text-muted-foreground">
                            External → Internal
                          </div>
                          <code className="text-sm font-mono">
                            443 → {window.location.hostname}:443
                          </code>
                        </div>
                      </div>
                    </div>
                  </div>

                  <Alert className="border-orange-200">
                    <AlertCircle className="h-4 w-4 text-orange-600" />
                    <AlertDescription>
                      <strong>Router Configuration Required:</strong>
                      <ol className="mt-2 space-y-1 text-sm list-decimal list-inside">
                        <li>
                          Access your router&apos;s admin panel (usually at
                          192.168.1.1 or 192.168.0.1)
                        </li>
                        <li>
                          Navigate to Port Forwarding or Virtual Server settings
                        </li>
                        <li>Add the port mappings shown above</li>
                        <li>Save and restart your router if necessary</li>
                      </ol>
                    </AlertDescription>
                  </Alert>

                  {getCurrentPort() !== '80' && getCurrentPort() !== '443' && (
                    <Alert className="border-yellow-200">
                      <Info className="h-4 w-4 text-yellow-600" />
                      <AlertDescription>
                        You&apos;re currently accessing on port{' '}
                        <strong>{getCurrentPort()}</strong>. For production use,
                        configure standard ports 80 and 443 for better
                        compatibility.
                      </AlertDescription>
                    </Alert>
                  )}
                </CardContent>
              </Card>
            )}

            <div className="flex items-center justify-between pt-4 border-t">
              <div className="space-y-1">
                <h4 className="font-medium">Need to reconfigure?</h4>
                <p className="text-sm text-muted-foreground">
                  If you need to change your external connectivity setup, you
                  can reconfigure it below.
                </p>
              </div>
              <Button
                variant="outline"
                onClick={() => setIsConfigured(false)}
                className="gap-2"
              >
                <Settings className="h-4 w-4" />
                Reconfigure Setup
              </Button>
            </div>

            <Button
              onClick={() =>
                onComplete ? onComplete() : navigate('/dashboard')
              }
              className="w-full"
            >
              Continue to Dashboard
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  if (currentStep === 'complete') {
    return (
      <div className="max-w-4xl mx-auto space-y-6">
        <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <CheckCircle2 className="h-6 w-6 text-green-600" />
              External Connectivity Setup Complete!
            </CardTitle>
            <CardDescription>
              Your platform is now configured for external access
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {networkSolution && (
              <div className="space-y-2">
                <h4 className="font-medium">Network Solution</h4>
                <Badge variant="outline">
                  {networkSolution === 'direct' && 'Direct Access'}
                  {networkSolution === 'port-forwarding' && 'Port Forwarding'}
                  {networkSolution === 'cloudflare-tunnel' &&
                    'Cloudflare Tunnel'}
                </Badge>
              </div>
            )}

            <div className="space-y-2">
              <h4 className="font-medium">Configured Domain</h4>
              <code className="block p-2 bg-muted rounded text-sm">
                {domainName}
              </code>
            </div>

            <Alert>
              <Info className="h-4 w-4" />
              <AlertDescription>
                Your external connectivity setup is complete. New deployments
                will be accessible at subdomains of your configured wildcard
                domain.
              </AlertDescription>
            </Alert>

            <Button
              onClick={() =>
                onComplete ? onComplete() : navigate('/dashboard')
              }
              className="w-full"
            >
              Continue to Dashboard
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <div>
        <h1 className="text-3xl font-bold tracking-tight">
          External Connectivity Setup
        </h1>
        <p className="text-muted-foreground mt-2">
          Configure your platform for external access with domain and network
          settings
        </p>
      </div>

      {/* Progress Indicator */}
      {networkSolution && (
        <div className="relative">
          <div className="flex items-center justify-between max-w-md mx-auto">
            <div className="flex flex-col items-center relative z-10">
              <div
                className={cn(
                  'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                  isStepComplete('detection')
                    ? 'bg-primary border-primary text-primary-foreground'
                    : 'border-muted bg-background text-muted-foreground'
                )}
              >
                {isStepComplete('detection') ? (
                  <CheckCircle2 className="h-5 w-5" />
                ) : (
                  <span className="text-sm font-semibold">1</span>
                )}
              </div>
              <span className="text-xs mt-2 font-medium">Setup Method</span>
            </div>

            <div className="flex flex-col items-center relative z-10">
              <div
                className={cn(
                  'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                  currentStep === 'domain'
                    ? 'border-primary bg-background text-primary'
                    : 'border-muted bg-background text-muted-foreground'
                )}
              >
                <span className="text-sm font-semibold">2</span>
              </div>
              <span className="text-xs mt-2 font-medium">Domain</span>
            </div>
          </div>

          {/* Progress Line */}
          <div className="absolute top-5 left-0 right-0 h-0.5 bg-muted -z-10">
            <div
              className="h-full bg-primary transition-all duration-500"
              style={{
                width: `${((getCurrentStepNumber() - 1) / 1) * 100}%`,
              }}
            />
          </div>
        </div>
      )}

      {/* Step 1: Auto-detection and Setup Options */}
      {currentStep === 'detection' && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              {accessLoading ? (
                <Loader2 className="h-5 w-5 animate-spin" />
              ) : (
                <Settings className="h-5 w-5" />
              )}
              Choose Your Setup Method
            </CardTitle>
            <CardDescription>
              {accessLoading ? (
                'Detecting your environment...'
              ) : accessInfo ? (
                <>
                  We detected:{' '}
                  <Badge variant="outline" className="ml-1">
                    {accessInfo.access_mode}
                  </Badge>
                  {accessInfo.public_ip && (
                    <span className="ml-2">({accessInfo.public_ip})</span>
                  )}
                </>
              ) : (
                'Select the best connectivity option for your environment'
              )}
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            {/* Recommended Options */}
            <div className="space-y-4">
              {setupOptions.length === 0 ? (
                <Alert>
                  <Loader2 className="h-4 w-4 animate-spin" />
                  <AlertDescription>
                    Analyzing your environment to provide the best setup
                    options...
                  </AlertDescription>
                </Alert>
              ) : (
                setupOptions.map((option) => (
                  <Card
                    key={option.id}
                    className={cn(
                      'cursor-pointer transition-all hover:border-primary hover:shadow-md',
                      option.recommended && 'border-primary/50 bg-primary/5'
                    )}
                    onClick={() => handleSolutionSelect(option.id)}
                  >
                    <CardContent className="p-6">
                      <div className="flex items-start gap-4">
                        <div className="mt-1">{option.icon}</div>
                        <div className="flex-1 space-y-3">
                          <div className="flex items-center gap-2">
                            <h4 className="font-semibold text-lg">
                              {option.title}
                            </h4>
                            {option.recommended && (
                              <Badge className="bg-blue-600">Recommended</Badge>
                            )}
                          </div>
                          <p className="text-sm text-muted-foreground">
                            {option.description}
                          </p>
                          <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pt-2">
                            <div className="space-y-1">
                              {option.pros.map((pro, i) => (
                                <div
                                  key={i}
                                  className="flex items-center gap-2 text-sm"
                                >
                                  <CheckCircle2 className="h-3 w-3 text-green-600 flex-shrink-0" />
                                  <span>{pro}</span>
                                </div>
                              ))}
                            </div>
                            <div className="space-y-1">
                              {option.cons.map((con, i) => (
                                <div
                                  key={i}
                                  className="flex items-center gap-2 text-sm text-muted-foreground"
                                >
                                  <Circle className="h-3 w-3 flex-shrink-0" />
                                  <span>{con}</span>
                                </div>
                              ))}
                            </div>
                          </div>
                        </div>
                      </div>
                    </CardContent>
                  </Card>
                ))
              )}
            </div>

            {/* Environment Info */}
            {accessInfo && (
              <Alert>
                <Info className="h-4 w-4" />
                <AlertDescription>
                  <strong>Current Environment:</strong> {accessInfo.access_mode}
                  {accessInfo.public_ip && (
                    <span className="block mt-1">
                      Public IP:{' '}
                      <code className="font-mono">{accessInfo.public_ip}</code>
                    </span>
                  )}
                </AlertDescription>
              </Alert>
            )}
          </CardContent>
        </Card>
      )}

      {/* Step 2: Domain Configuration */}
      {currentStep === 'domain' && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Globe className="h-5 w-5" />
              Domain Configuration
            </CardTitle>
            <CardDescription>
              Configure your wildcard domain for dynamic project URLs
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            {/* Selected Method Badge */}
            {networkSolution && (
              <div className="flex items-center gap-2">
                <span className="text-sm text-muted-foreground">
                  Setup Method:
                </span>
                <Badge variant="outline">
                  {networkSolution === 'direct' && 'Direct Access'}
                  {networkSolution === 'port-forwarding' && 'Port Forwarding'}
                  {networkSolution === 'cloudflare-tunnel' &&
                    'Cloudflare Tunnel'}
                </Badge>
              </div>
            )}

            {/* Public IP Verification for Direct Access */}
            {networkSolution === 'direct' && (
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <Label className="text-base font-medium">
                    Public IP Verification
                  </Label>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => refetchIp()}
                    disabled={isLoadingIp}
                  >
                    {isLoadingIp ? (
                      <>
                        <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                        Checking...
                      </>
                    ) : (
                      <>
                        <RefreshCw className="mr-2 h-4 w-4" />
                        Refresh
                      </>
                    )}
                  </Button>
                </div>

                {isLoadingIp || accessLoading ? (
                  <div className="flex items-center gap-2 p-3 border rounded-lg">
                    <Loader2 className="h-4 w-4 animate-spin" />
                    <span>Detecting public IP...</span>
                  </div>
                ) : effectivePublicIP || publicIpData ? (
                  <div className="p-4 border rounded-lg bg-green-50 dark:bg-green-950/20 space-y-2">
                    <div className="flex items-center gap-2">
                      <CheckCircle2 className="h-4 w-4 text-green-600" />
                      <span className="font-medium text-green-900 dark:text-green-100">
                        Public IP Detected
                      </span>
                    </div>
                    <div className="flex items-center justify-between bg-white dark:bg-gray-900 p-3 rounded-md border">
                      <div className="space-y-1">
                        <div className="flex items-center gap-2">
                          <span className="text-sm text-muted-foreground">
                            Source:
                          </span>
                          <span className="text-sm font-medium">
                            Server Public IP
                          </span>
                        </div>
                        <div className="flex items-center gap-2">
                          <span className="text-sm text-muted-foreground">
                            Address:
                          </span>
                          <code className="text-lg font-mono font-semibold">
                            {effectivePublicIP || 'Unable to detect'}
                          </code>
                        </div>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => copyToClipboard(effectivePublicIP || '')}
                        disabled={!effectivePublicIP}
                      >
                        <Copy className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>
                ) : (
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      Could not detect public IP. Please verify your server has
                      internet access.
                    </AlertDescription>
                  </Alert>
                )}

                <div className="flex items-center space-x-2">
                  <Switch
                    id="confirm-access"
                    checked={confirmedPublicAccess}
                    onCheckedChange={setConfirmedPublicAccess}
                  />
                  <Label htmlFor="confirm-access" className="text-sm">
                    I confirm this server is publicly accessible from the
                    internet
                  </Label>
                </div>
              </div>
            )}

            {/* Network Setup Instructions (Collapsible) */}
            {networkSolution && networkSolution !== 'direct' && (
              <Collapsible
                open={setupInstructionsOpen}
                onOpenChange={setSetupInstructionsOpen}
              >
                <CollapsibleTrigger asChild>
                  <Button variant="outline" className="w-full justify-between">
                    <span className="flex items-center gap-2">
                      {networkSolution === 'port-forwarding' && (
                        <Router className="h-4 w-4" />
                      )}
                      {networkSolution === 'cloudflare-tunnel' && (
                        <Cloud className="h-4 w-4" />
                      )}
                      <span className="font-medium">
                        {networkSolution === 'port-forwarding' &&
                          'Port Forwarding Setup Instructions'}
                        {networkSolution === 'cloudflare-tunnel' &&
                          'Cloudflare Tunnel Setup Instructions'}
                      </span>
                    </span>
                    <ChevronDown
                      className={cn(
                        'h-4 w-4 transition-transform',
                        setupInstructionsOpen && 'rotate-180'
                      )}
                    />
                  </Button>
                </CollapsibleTrigger>
                <CollapsibleContent className="pt-4 space-y-4">
                  {networkSolution === 'port-forwarding' && (
                    <div className="space-y-4">
                      <Alert>
                        <Router className="h-4 w-4" />
                        <AlertDescription>
                          <strong>
                            Configure your router to forward these ports:
                          </strong>
                          <ul className="mt-2 space-y-1 text-sm list-disc list-inside">
                            <li>Forward port 80 (HTTP) to this server</li>
                            <li>Forward port 443 (HTTPS) to this server</li>
                            <li>
                              Set a static IP for this server on your local
                              network
                            </li>
                          </ul>
                        </AlertDescription>
                      </Alert>

                      {showPortMapping && (
                        <Card className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                          <CardHeader className="pb-3">
                            <CardTitle className="text-base flex items-center gap-2">
                              <Network className="h-4 w-4 text-orange-600" />
                              Specific Port Mapping Configuration
                            </CardTitle>
                          </CardHeader>
                          <CardContent className="space-y-3">
                            <div className="text-sm text-muted-foreground">
                              Configure these exact mappings in your router:
                            </div>
                            <div className="space-y-2">
                              <div className="flex items-center justify-between p-2 bg-white dark:bg-gray-900 rounded border text-sm">
                                <span className="font-medium">HTTP:</span>
                                <code className="font-mono">
                                  External Port 80 → {window.location.hostname}
                                  :80
                                </code>
                              </div>
                              <div className="flex items-center justify-between p-2 bg-white dark:bg-gray-900 rounded border text-sm">
                                <span className="font-medium">HTTPS:</span>
                                <code className="font-mono">
                                  External Port 443 → {window.location.hostname}
                                  :443
                                </code>
                              </div>
                            </div>
                            <div className="text-xs text-muted-foreground mt-2">
                              Your server&apos;s private IP:{' '}
                              <code className="font-mono font-semibold">
                                {window.location.hostname}
                              </code>
                            </div>
                          </CardContent>
                        </Card>
                      )}
                    </div>
                  )}

                  {networkSolution === 'cloudflare-tunnel' && (
                    <div className="space-y-4">
                      <Alert>
                        <Cloud className="h-4 w-4" />
                        <AlertDescription>
                          <strong>Cloudflare Tunnel:</strong> Follow these steps
                          to set up a secure tunnel. You must use a base domain
                          (not subdomain) as wildcards only work on the base
                          domain level.
                        </AlertDescription>
                      </Alert>

                      {/* Cloudflare Tunnel Setup Instructions */}
                      <div className="space-y-3">
                        <Label>Setup Instructions</Label>
                        <Card className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
                          <CardContent className="pt-6 space-y-4">
                            <div className="space-y-3">
                              <h4 className="font-medium flex items-center gap-2">
                                <span className="flex items-center justify-center w-6 h-6 rounded-full bg-blue-600 text-white text-xs font-bold">
                                  1
                                </span>
                                Install cloudflared
                              </h4>
                              <div className="ml-8 space-y-2">
                                <CodeBlock
                                  language="bash"
                                  code={`# macOS
brew install cloudflared jq

# Linux - download and install both tools
curl -L https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64 -o cloudflared && chmod +x cloudflared && sudo mv cloudflared /usr/local/bin/
sudo apt-get install jq  # or yum/dnf for other distros`}
                                />
                              </div>
                            </div>

                            <div className="space-y-3">
                              <h4 className="font-medium flex items-center gap-2">
                                <span className="flex items-center justify-center w-6 h-6 rounded-full bg-blue-600 text-white text-xs font-bold">
                                  2
                                </span>
                                Authenticate with your domain
                              </h4>
                              <div className="ml-8 space-y-2">
                                <CodeBlock
                                  language="bash"
                                  code="cloudflared tunnel login"
                                />
                                <div className="p-2 bg-amber-50 dark:bg-amber-950/30 rounded text-xs">
                                  <p className="text-amber-900 dark:text-amber-100">
                                    ⚠️ Select the domain{' '}
                                    <InlineCode>{baseDomain}</InlineCode> when
                                    prompted in your browser
                                  </p>
                                </div>
                              </div>
                            </div>

                            <div className="space-y-3">
                              <h4 className="font-medium flex items-center gap-2">
                                <span className="flex items-center justify-center w-6 h-6 rounded-full bg-blue-600 text-white text-xs font-bold">
                                  3
                                </span>
                                Create tunnel and configure DNS
                              </h4>
                              <div className="ml-8 space-y-2">
                                <CodeBlock
                                  language="bash"
                                  code={`# Create tunnel and save ID
TUNNEL_JSON=$(cloudflared tunnel create --output json ${tunnelName})
TUNNEL_ID=$(echo "$TUNNEL_JSON" | jq -r '.id') && echo "Tunnel ID: $TUNNEL_ID"

# Configure DNS for both wildcard and base domain
cloudflared tunnel route dns ${tunnelName} "*.${baseDomain || 'example.com'}"
cloudflared tunnel route dns ${tunnelName} "${baseDomain || 'example.com'}"`}
                                />
                                <div className="p-2 bg-blue-50 dark:bg-blue-950/30 rounded text-xs">
                                  <p className="text-blue-900 dark:text-blue-100">
                                    💡 Save the Tunnel ID - you&apos;ll need it
                                    for the config file
                                  </p>
                                </div>
                              </div>
                            </div>

                            <div className="space-y-3">
                              <h4 className="font-medium flex items-center gap-2">
                                <span className="flex items-center justify-center w-6 h-6 rounded-full bg-blue-600 text-white text-xs font-bold">
                                  4
                                </span>
                                Create configuration file
                              </h4>
                              <div className="ml-8 space-y-2">
                                <CodeBlock
                                  language="bash"
                                  code={`# Backup any existing config and create new one
[ -f ~/.cloudflared/config.yml ] && mv ~/.cloudflared/config.yml ~/.cloudflared/config_old_$(date +%Y%m%d_%H%M%S).yml

# Create config file
cat > ~/.cloudflared/config.yml << EOF
tunnel: ${tunnelName}
credentials-file: ~/.cloudflared/$TUNNEL_ID.json
ingress:
  - hostname: "*.${baseDomain || 'example.com'}"
    service: http://localhost:80
  - hostname: "${baseDomain || 'example.com'}"
    service: http://localhost:80
  - service: http_status:404
EOF`}
                                />
                              </div>
                            </div>

                            <div className="space-y-3">
                              <h4 className="font-medium flex items-center gap-2">
                                <span className="flex items-center justify-center w-6 h-6 rounded-full bg-blue-600 text-white text-xs font-bold">
                                  5
                                </span>
                                Verify and run the tunnel
                              </h4>
                              <div className="ml-8 space-y-2">
                                <CodeBlock
                                  language="bash"
                                  code={`# Verify your setup
cloudflared tunnel list
echo "Your domain: *.${baseDomain || 'example.com'}"

# Run the tunnel
cloudflared tunnel run ${tunnelName}`}
                                />
                                <p className="text-xs text-muted-foreground">
                                  For production, install as a service:
                                </p>
                                <CodeBlock
                                  language="bash"
                                  code={`sudo cloudflared service install
sudo systemctl start cloudflared`}
                                />
                              </div>
                            </div>
                          </CardContent>
                        </Card>
                      </div>

                      <Alert className="mt-3">
                        <CheckCircle2 className="h-4 w-4 text-green-600" />
                        <AlertDescription>
                          <strong>Your Configuration:</strong>
                          <div className="mt-2 space-y-1 text-sm font-mono">
                            <div>
                              🌐 Wildcard Domain:{' '}
                              <span className="font-semibold">
                                *.{baseDomain}
                              </span>
                            </div>
                            <div>
                              🔧 Tunnel Name:{' '}
                              <span className="font-semibold">
                                {tunnelName}
                              </span>
                            </div>
                            <div>
                              📍 Target:{' '}
                              <span className="font-semibold">
                                localhost:80
                              </span>
                            </div>
                          </div>
                        </AlertDescription>
                      </Alert>

                      <div className="space-y-3 mt-4">
                        <Label>Quick Setup Script</Label>
                        <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                          <CardContent className="pt-6">
                            <p className="text-xs text-muted-foreground mb-3">
                              One-liner to set everything up:
                            </p>
                            <CodeBlock
                              language="bash"
                              code={`#!/bin/bash
DOMAIN="${baseDomain || 'example.com'}"
TUNNEL_NAME="${tunnelName}"

# Create tunnel and configure DNS for both wildcard and base domain
TUNNEL_ID=$(cloudflared tunnel create --output json $TUNNEL_NAME | jq -r '.id')
cloudflared tunnel route dns $TUNNEL_NAME "*.$DOMAIN"
cloudflared tunnel route dns $TUNNEL_NAME "$DOMAIN"

# Create config
cat > ~/.cloudflared/config.yml << EOF
tunnel: $TUNNEL_NAME
credentials-file: ~/.cloudflared/$TUNNEL_ID.json
ingress:
  - hostname: "*.$DOMAIN"
    service: http://localhost:80
  - hostname: "$DOMAIN"
    service: http://localhost:80
  - service: http_status:404
EOF

# Run tunnel
cloudflared tunnel run $TUNNEL_NAME

# Step 2: Create tunnel and capture ID
echo "Creating tunnel: $TUNNEL_NAME"

# Check if tunnel already exists
EXISTING_ID=$(cloudflared tunnel list --output json | jq -r '.[] | select(.name=="'$TUNNEL_NAME'") | .id')

if [ ! -z "$EXISTING_ID" ]; then
  echo "Tunnel already exists with ID: $EXISTING_ID"
  TUNNEL_ID=$EXISTING_ID
else
  # Create new tunnel
  TUNNEL_JSON=$(cloudflared tunnel create $TUNNEL_NAME --output json 2>/dev/null)
  if [ $? -eq 0 ]; then
    TUNNEL_ID=$(echo "$TUNNEL_JSON" | jq -r '.id')
    echo "Created new tunnel with ID: $TUNNEL_ID"
  else
    echo "Error creating tunnel"
    exit 1
  fi
fi

echo "Tunnel ID: $TUNNEL_ID"

# Step 3: Configure DNS
echo "Configuring DNS for *.$DOMAIN..."
cloudflared tunnel route dns $TUNNEL_ID "*.$DOMAIN"

# Step 4: Create config file
CONFIG_FILE=~/.cloudflared/config.yml

# Backup existing config if it exists
if [ -f $CONFIG_FILE ]; then
  BACKUP_FILE=~/.cloudflared/config_old_$(date +%Y%m%d_%H%M%S).yml
  mv $CONFIG_FILE $BACKUP_FILE
  echo "Backed up existing config to $BACKUP_FILE"
fi

echo "Creating config file at $CONFIG_FILE..."
cat > $CONFIG_FILE <<EOF
tunnel: $TUNNEL_NAME
credentials-file: ~/.cloudflared/$TUNNEL_ID.json

ingress:
  - hostname: "*.$DOMAIN"
    service: http://localhost:80
  - hostname: "$DOMAIN"
    service: http://localhost:80
  - service: http_status:404
EOF

echo "✅ Tunnel setup complete!"
echo "Run 'cloudflared tunnel run $TUNNEL_NAME' to start the tunnel"`}
                            />
                          </CardContent>
                        </Card>
                      </div>
                    </div>
                  )}
                </CollapsibleContent>
              </Collapsible>
            )}

            {/* Domain Input */}
            {networkSolution === 'cloudflare-tunnel' && (
              <div className="space-y-3">
                <Label>Base Domain (Required for Cloudflare Tunnel)</Label>
                <Input
                  value={baseDomain}
                  onChange={(e) => {
                    setBaseDomain(e.target.value)
                    setDomainName(`*.${e.target.value}`)
                  }}
                  placeholder="example.com"
                />
                <p className="text-xs text-muted-foreground">
                  Enter your base domain (e.g., example.com). The wildcard will
                  be *.{baseDomain}
                </p>
              </div>
            )}

            {/* Wildcard Domain Input */}
            <div className="space-y-3">
              <Label>Wildcard Domain</Label>
              <Input
                value={domainName}
                onChange={(e) => setDomainName(e.target.value)}
                placeholder="*.yourdomain.com"
                disabled={networkSolution === 'cloudflare-tunnel'}
              />
              <p className="text-xs text-muted-foreground">
                {networkSolution === 'cloudflare-tunnel'
                  ? 'Domain automatically set based on base domain above'
                  : 'Must start with *. (e.g., *.example.com)'}
              </p>
            </div>

            {/* DNS Configuration Instructions */}
            <Alert>
              <Info className="h-4 w-4" />
              <AlertDescription>
                <strong>DNS Configuration:</strong>
                <ul className="mt-2 space-y-1 text-sm list-disc list-inside">
                  {networkSolution === 'direct' && (
                    <>
                      <li>
                        Add an A record for *.yourdomain.com pointing to{' '}
                        {effectivePublicIP || 'your server IP'}
                      </li>
                      <li>
                        Or add a CNAME record pointing to your server hostname
                      </li>
                    </>
                  )}
                  {networkSolution === 'port-forwarding' && (
                    <>
                      <li>
                        Add an A record for *.yourdomain.com pointing to your
                        public IP
                      </li>
                      <li>
                        Ensure ports 80 and 443 are forwarded to this server
                      </li>
                    </>
                  )}
                  {networkSolution === 'cloudflare-tunnel' && (
                    <>
                      <li>
                        DNS will be automatically configured through Cloudflare
                      </li>
                      <li>Ensure your domain is managed by Cloudflare</li>
                    </>
                  )}
                  <li>
                    SSL certificates will be automatically provisioned via
                    Let&apos;s Encrypt
                  </li>
                </ul>
              </AlertDescription>
            </Alert>

            <div className="flex justify-end space-x-2">
              <Button
                variant="outline"
                onClick={() => {
                  setCurrentStep('detection')
                  setNetworkSolution(null)
                }}
              >
                Back
              </Button>
              <Button
                onClick={handleDomainSubmit}
                disabled={
                  createDomain.isPending ||
                  !domainName ||
                  !domainName.startsWith('*.') ||
                  (networkSolution === 'direct' && !effectivePublicIP) ||
                  (networkSolution === 'cloudflare-tunnel' && !baseDomain)
                }
              >
                {createDomain.isPending ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    Configuring...
                  </>
                ) : (
                  <>
                    <Globe className="mr-2 h-4 w-4" />
                    Complete Setup
                  </>
                )}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}

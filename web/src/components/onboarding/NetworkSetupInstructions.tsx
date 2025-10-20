import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { CodeBlock } from '@/components/ui/code-block'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { Info, AlertTriangle, Server, Router, Cloud, Copy } from 'lucide-react'
import { NetworkMode } from './NetworkModeSelector'
import { usePlatformAccess } from '@/contexts/PlatformAccessContext'
import { toast } from 'sonner'

interface NetworkSetupInstructionsProps {
  networkMode: NetworkMode
  baseDomain: string
  onNext: () => void
  onBack: () => void
}

export function NetworkSetupInstructions({
  networkMode,
  baseDomain,
  onNext,
  onBack,
}: NetworkSetupInstructionsProps) {
  const { accessInfo } = usePlatformAccess()
  const [confirmed, setConfirmed] = useState(false)

  // Get public IP from access info, show placeholder if not available
  const publicIp = accessInfo?.public_ip
  const publicIpDisplay = publicIp || 'YOUR_PUBLIC_IP'
  const slugifiedDomain = baseDomain
    .toLowerCase()
    .replace(/\./g, '-')
    .replace(/[^a-z0-9-]/g, '')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
  const tunnelName = `${slugifiedDomain}-temps`

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text)
    toast.success('Copied to clipboard')
  }

  const renderInstructions = () => {
    switch (networkMode) {
      case 'direct':
        return (
          <div className="space-y-4">
            <Alert>
              <Server className="h-4 w-4" />
              <AlertTitle>Direct/VPS Setup</AlertTitle>
              <AlertDescription>
                Configure your DNS records to point to your server&apos;s public IP
              </AlertDescription>
            </Alert>

            <div className="space-y-4">
              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    1
                  </span>
                  Add DNS A Record for Wildcard Domain
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground">
                    Add the following DNS records in your domain registrar:
                  </p>
                  <div className="bg-muted p-3 rounded-lg font-mono text-sm space-y-1">
                    <div className="flex items-center justify-between">
                      <div>
                        <span className="text-muted-foreground">Type:</span>{' '}
                        <span className="font-semibold">A</span>
                      </div>
                    </div>
                    <div className="flex items-center justify-between">
                      <div>
                        <span className="text-muted-foreground">Name:</span>{' '}
                        <span className="font-semibold">*.{baseDomain}</span>
                      </div>
                    </div>
                    <div className="flex items-center justify-between">
                      <div>
                        <span className="text-muted-foreground">Value:</span>{' '}
                        <span className="font-semibold">{publicIpDisplay}</span>
                      </div>
                      {publicIp && (
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => copyToClipboard(publicIp)}
                        >
                          <Copy className="h-3 w-3" />
                        </Button>
                      )}
                    </div>
                  </div>
                  {publicIp && (
                    <Alert className="mt-2 border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                      <Info className="h-4 w-4 text-orange-600" />
                      <AlertDescription className="text-xs">
                        <strong>Note:</strong> Your public IP ({publicIp}) may
                        change if you restart your server or if your ISP assigns
                        a new IP. Consider using a static IP or DNS service for
                        production.
                      </AlertDescription>
                    </Alert>
                  )}
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    2
                  </span>
                  Wait for DNS Propagation
                </h4>
                <div className="ml-8">
                  <p className="text-sm text-muted-foreground">
                    DNS changes can take 5-60 minutes to propagate globally. You
                    can check propagation status at{' '}
                    <a
                      href="https://www.whatsmydns.net"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-primary hover:underline"
                    >
                      whatsmydns.net
                    </a>
                  </p>
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    3
                  </span>
                  Ensure Firewall Rules
                </h4>
                <div className="ml-8">
                  <p className="text-sm text-muted-foreground mb-2">
                    Make sure your firewall allows incoming traffic on ports:
                  </p>
                  <ul className="text-sm text-muted-foreground space-y-1">
                    <li>• Port 80 (HTTP) - for Let&apos;s Encrypt validation</li>
                    <li>• Port 443 (HTTPS) - for secure connections</li>
                  </ul>
                </div>
              </div>
            </div>

            <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
              <Info className="h-4 w-4 text-blue-600" />
              <AlertDescription>
                After DNS propagation, we&apos;ll automatically provision SSL
                certificates using Let&apos;s Encrypt.
              </AlertDescription>
            </Alert>
          </div>
        )

      case 'nat':
        return (
          <div className="space-y-4">
            <Alert>
              <Router className="h-4 w-4" />
              <AlertTitle>NAT/Port Forwarding Setup</AlertTitle>
              <AlertDescription>
                Configure your router to forward ports 80 and 443
              </AlertDescription>
            </Alert>

            <div className="space-y-4">
              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    1
                  </span>
                  Find Your Router&apos;s IP Address
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground">
                    Usually one of: 192.168.1.1, 192.168.0.1, or 10.0.0.1
                  </p>
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    2
                  </span>
                  Configure Port Forwarding
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground">
                    Access your router&apos;s admin panel and add port forwarding
                    rules:
                  </p>
                  <div className="bg-muted p-3 rounded-lg text-sm space-y-2">
                    <div className="font-medium">Rule 1: HTTP</div>
                    <div className="ml-4 space-y-1 font-mono text-xs">
                      <div>External Port: 80</div>
                      <div>Internal Port: 80</div>
                      <div>
                        Internal IP:{' '}
                        {accessInfo?.private_ip || 'YOUR_SERVER_IP'}
                      </div>
                      <div>Protocol: TCP</div>
                    </div>
                    <div className="font-medium mt-3">Rule 2: HTTPS</div>
                    <div className="ml-4 space-y-1 font-mono text-xs">
                      <div>External Port: 443</div>
                      <div>Internal Port: 443</div>
                      <div>
                        Internal IP:{' '}
                        {accessInfo?.private_ip || 'YOUR_SERVER_IP'}
                      </div>
                      <div>Protocol: TCP</div>
                    </div>
                  </div>
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    3
                  </span>
                  Add DNS A Record
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground">
                    Point your wildcard domain to your public IP:
                  </p>
                  <div className="bg-muted p-3 rounded-lg font-mono text-sm">
                    <div>Type: A</div>
                    <div>Name: *.{baseDomain}</div>
                    <div>Value: {publicIpDisplay}</div>
                  </div>
                  {publicIp && (
                    <Alert className="mt-2 border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                      <Info className="h-4 w-4 text-orange-600" />
                      <AlertDescription className="text-xs">
                        <strong>Note:</strong> Your public IP ({publicIp}) may
                        change if you restart your server or if your ISP assigns
                        a new IP. Consider using a dynamic DNS service for
                        production.
                      </AlertDescription>
                    </Alert>
                  )}
                </div>
              </div>
            </div>

            <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
              <AlertTriangle className="h-4 w-4 text-orange-600" />
              <AlertDescription>
                <strong>Note:</strong> Some ISPs use CGNAT which prevents port
                forwarding. If this doesn&apos;t work, consider Cloudflare Tunnel
                instead.
              </AlertDescription>
            </Alert>
          </div>
        )

      case 'cloudflare':
        const isSubdomain = baseDomain.split('.').length > 2
        return (
          <div className="space-y-4">
            <Alert>
              <Cloud className="h-4 w-4" />
              <AlertTitle>Cloudflare Tunnel Setup</AlertTitle>
              <AlertDescription>
                Install and configure Cloudflare Tunnel for secure access
              </AlertDescription>
            </Alert>

            {isSubdomain && (
              <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                <AlertTriangle className="h-4 w-4 text-orange-600" />
                <AlertTitle>Wildcard Domain Limitation</AlertTitle>
                <AlertDescription>
                  <p className="mb-2">
                    Cloudflare Tunnel requires wildcard domains to be at the
                    <strong> root level</strong> of your domain.
                  </p>
                  <p className="text-sm">
                    Your domain <code className="font-mono">{baseDomain}</code>{' '}
                    appears to be a subdomain. For wildcard support, please use
                    the root domain (e.g., example.com instead of
                    sub.example.com)
                  </p>
                </AlertDescription>
              </Alert>
            )}

            <div className="space-y-4">
              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    1
                  </span>
                  Install cloudflared
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground mb-2">
                    Install cloudflared on your server:
                  </p>
                  <CodeBlock
                    language="bash"
                    code={`# For Ubuntu/Debian
curl -L --output cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64.deb
sudo dpkg -i cloudflared.deb

# For macOS
brew install cloudflared`}
                  />
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    2
                  </span>
                  Authenticate with Cloudflare
                </h4>
                <div className="ml-8 space-y-2">
                  <CodeBlock
                    language="bash"
                    code={`cloudflared tunnel login`}
                  />
                  <p className="text-sm text-muted-foreground">
                    This will open a browser window to authenticate
                  </p>
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    3
                  </span>
                  Create Tunnel
                </h4>
                <div className="ml-8 space-y-2">
                  <CodeBlock
                    language="bash"
                    code={`cloudflared tunnel create ${tunnelName}`}
                  />
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    4
                  </span>
                  Configure Tunnel
                </h4>
                <div className="ml-8 space-y-2">
                  <p className="text-sm text-muted-foreground mb-2">
                    Create a config file at{' '}
                    <code className="font-mono text-xs bg-muted px-1 py-0.5 rounded">
                      ~/.cloudflared/config.yml
                    </code>
                    :
                  </p>
                  <CodeBlock
                    language="yaml"
                    code={`tunnel: ${tunnelName}
credentials-file: /root/.cloudflared/<TUNNEL_ID>.json

ingress:
  - hostname: "*.${baseDomain}"
    service: https://localhost:443
    originRequest:
      noTLSVerify: true
  - service: http_status:404`}
                  />
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    5
                  </span>
                  Route DNS to Tunnel
                </h4>
                <div className="ml-8 space-y-2">
                  <CodeBlock
                    language="bash"
                    code={`cloudflared tunnel route dns ${tunnelName} "*.${baseDomain}"`}
                  />
                </div>
              </div>

              <div>
                <h4 className="font-medium mb-2 flex items-center gap-2">
                  <span className="flex h-6 w-6 items-center justify-center rounded-full bg-primary text-xs text-primary-foreground">
                    6
                  </span>
                  Start Tunnel
                </h4>
                <div className="ml-8 space-y-2">
                  <CodeBlock
                    language="bash"
                    code={`cloudflared tunnel run ${tunnelName}`}
                  />
                  <p className="text-sm text-muted-foreground">
                    For production, set up as a system service
                  </p>
                </div>
              </div>
            </div>
          </div>
        )

      default:
        return null
    }
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Network Setup Instructions</h2>
        <p className="text-muted-foreground">
          Follow these steps to configure your {networkMode} setup
        </p>
      </div>

      <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
        <Info className="h-4 w-4 text-blue-600" />
        <AlertDescription>
          <strong>Important:</strong> Wildcard domains (*.{baseDomain}) require
          DNS configuration. You cannot use direct IP access for wildcard
          certificates. Make sure you have access to your domain&apos;s DNS settings.
        </AlertDescription>
      </Alert>

      {renderInstructions()}

      <div className="flex items-center space-x-2 pt-4 border-t">
        <Checkbox
          id="confirm-setup"
          checked={confirmed}
          onCheckedChange={(checked) => setConfirmed(checked as boolean)}
        />
        <Label htmlFor="confirm-setup" className="text-sm font-normal">
          I have completed the network setup steps above
        </Label>
      </div>

      <div className="flex justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={onNext} disabled={!confirmed}>
          Continue to Domain Challenge
        </Button>
      </div>
    </div>
  )
}

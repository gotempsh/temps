import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { ArrowRight, ArrowLeft, Globe, Info } from 'lucide-react'

interface GitHubAppConfigurationProps {
  domain: string
  onBack?: () => void
  onContinue: (publicUrl: string) => void
}

export function GitHubAppConfiguration({
  domain: _domain,
  onBack,
  onContinue,
}: GitHubAppConfigurationProps) {
  const [publicUrl, setPublicUrl] = useState('')
  const [error, setError] = useState('')

  const validateUrl = (url: string) => {
    if (!url) {
      setError('Public URL is required')
      return false
    }

    try {
      const urlObj = new URL(url)
      if (urlObj.protocol !== 'https:' && urlObj.protocol !== 'http:') {
        setError('URL must start with http:// or https://')
        return false
      }
      setError('')
      return true
    } catch {
      setError('Please enter a valid URL')
      return false
    }
  }

  const handleContinue = () => {
    if (validateUrl(publicUrl)) {
      onContinue(publicUrl)
    }
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-2xl font-bold">Configure GitHub App</h2>
        <p className="text-muted-foreground mt-2">
          Set up your public URL for GitHub App integration
        </p>
      </div>

      <Alert>
        <Info className="h-4 w-4" />
        <AlertDescription>
          GitHub requires a public URL to send webhooks. This should be the URL
          where your Temps instance is accessible from the internet.
        </AlertDescription>
      </Alert>

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <Card>
          <CardHeader>
            <CardTitle className="text-lg">What is a Public URL?</CardTitle>
            <CardDescription>
              Your Temps instance needs to be accessible from the internet for
              GitHub to send webhook events
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <h4 className="font-medium text-sm">Common Examples:</h4>
              <ul className="text-sm text-muted-foreground space-y-1">
                <li>• https://temps.yourdomain.com</li>
                <li>• https://yourdomain.com</li>
                <li>• https://subdomain.yourdomain.com</li>
              </ul>
            </div>

            <div className="space-y-2">
              <h4 className="font-medium text-sm">
                Options if you don&apos;t have one:
              </h4>
              <ul className="text-sm text-muted-foreground space-y-1">
                <li>• Use ngrok for temporary public URLs</li>
                <li>• Deploy to a cloud provider</li>
                <li>• Use Cloudflare Tunnels</li>
                <li>• Configure port forwarding on your router</li>
              </ul>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-lg">Public URL Configuration</CardTitle>
            <CardDescription>
              Enter the URL where your Temps instance is accessible
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="public-url">
                <Globe className="inline-block h-4 w-4 mr-1" />
                Public URL
              </Label>
              <Input
                id="public-url"
                type="url"
                placeholder="https://temps.yourdomain.com"
                value={publicUrl}
                onChange={(e) => {
                  setPublicUrl(e.target.value)
                  if (error) validateUrl(e.target.value)
                }}
                onBlur={() => publicUrl && validateUrl(publicUrl)}
                className={error ? 'border-destructive' : ''}
              />
              {error && <p className="text-sm text-destructive">{error}</p>}
              <p className="text-xs text-muted-foreground">
                This URL will be used for GitHub webhooks and OAuth callbacks
              </p>
            </div>

            <Alert variant="default" className="bg-muted">
              <AlertDescription className="text-sm">
                <strong>Important:</strong> Make sure this URL is accessible
                from the internet and points to your Temps instance.
              </AlertDescription>
            </Alert>
          </CardContent>
        </Card>
      </div>

      <div className="flex justify-between">
        {onBack && (
          <Button variant="outline" onClick={onBack}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back
          </Button>
        )}
        <Button
          onClick={handleContinue}
          disabled={!publicUrl || !!error}
          className="ml-auto"
        >
          Create GitHub App
          <ArrowRight className="ml-2 h-4 w-4" />
        </Button>
      </div>
    </div>
  )
}

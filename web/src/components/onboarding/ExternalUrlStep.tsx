import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Globe, Loader2, CheckCircle2, Info } from 'lucide-react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { updateSettingsMutation } from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'
import { DomainResponse } from '@/api/client/types.gen'

interface ExternalUrlStepProps {
  baseDomain: string
  domain: DomainResponse | null
  onSuccess: () => void
  onBack: () => void
}

export function ExternalUrlStep({
  baseDomain,
  domain,
  onSuccess,
  onBack,
}: ExternalUrlStepProps) {
  const queryClient = useQueryClient()
  const [externalUrl, setExternalUrl] = useState(`https://temps.${baseDomain}`)
  // Preview domain should be the base domain without the wildcard prefix
  const [previewDomain, setPreviewDomain] = useState(baseDomain)

  const updateSettings = useMutation({
    ...updateSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update settings',
    },
    onSuccess: (data) => {
      toast.success('External URL and preview domain configured!')
      // Invalidate settings queries to refresh
      queryClient.invalidateQueries({ queryKey: ['getSettings'] })
      queryClient.invalidateQueries({ queryKey: ['platform-settings'] })

      // Also update localStorage immediately
      const storedSettings = localStorage.getItem('platform_settings')
      if (storedSettings) {
        try {
          const settings = JSON.parse(storedSettings)
          settings.external_url = externalUrl
          settings.preview_domain = previewDomain
          localStorage.setItem('platform_settings', JSON.stringify(settings))
        } catch (e) {
          console.error('Failed to update localStorage settings:', e)
        }
      }

      onSuccess()
    },
  })

  const handleSave = () => {
    updateSettings.mutate({
      body: {
        external_url: externalUrl,
        preview_domain: previewDomain,
      },
    })
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Configure Access URLs</h2>
        <p className="text-muted-foreground">
          Set up your external URL and preview domain
        </p>
      </div>

      {domain && domain.status === 'valid' && (
        <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
          <CheckCircle2 className="h-4 w-4 text-green-600" />
          <AlertDescription>
            <strong>Domain verified!</strong> Your SSL certificate has been
            provisioned and is ready to use.
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="externalUrl">External URL</Label>
          <div className="relative">
            <Globe className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <Input
              id="externalUrl"
              value={externalUrl}
              onChange={(e) => setExternalUrl(e.target.value)}
              placeholder="https://temps.example.com"
              className="pl-10"
            />
          </div>
          <p className="text-xs text-muted-foreground">
            The main URL where you'll access the Temps dashboard
          </p>
        </div>

        <div className="space-y-2">
          <Label htmlFor="previewDomain">Preview Domain</Label>
          <div className="relative">
            <Globe className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
            <Input
              id="previewDomain"
              value={previewDomain}
              onChange={(e) => setPreviewDomain(e.target.value)}
              placeholder="example.com"
              className="pl-10"
            />
          </div>
          <p className="text-xs text-muted-foreground">
            Base domain for deployments (without wildcard)
          </p>
        </div>

        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription>
            <div className="space-y-2 text-sm">
              <p className="font-medium">Your deployments will use:</p>
              <ul className="space-y-1 ml-4">
                <li>
                  • Main dashboard:{' '}
                  <code className="font-mono text-xs bg-muted px-1 py-0.5 rounded">
                    {externalUrl}
                  </code>
                </li>
                <li>
                  • Project deployments:{' '}
                  <code className="font-mono text-xs bg-muted px-1 py-0.5 rounded">
                    project-name.{baseDomain}
                  </code>
                </li>
                <li>
                  • Preview builds:{' '}
                  <code className="font-mono text-xs bg-muted px-1 py-0.5 rounded">
                    pr-123.{baseDomain}
                  </code>
                </li>
              </ul>
            </div>
          </AlertDescription>
        </Alert>
      </div>

      <div className="flex justify-between">
        <Button variant="outline" onClick={onBack}>
          Back
        </Button>
        <Button onClick={handleSave} disabled={updateSettings.isPending}>
          {updateSettings.isPending ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              Saving...
            </>
          ) : (
            'Save & Continue'
          )}
        </Button>
      </div>
    </div>
  )
}

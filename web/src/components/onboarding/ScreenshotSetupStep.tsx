import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Camera, Info, Loader2, CheckCircle2 } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  getPlatformSettings,
  updatePlatformSettings,
} from '@/api/platformSettings'
import { toast } from 'sonner'

interface ScreenshotSetupStepProps {
  onNext: () => void
  onSkip: () => void
}

export function ScreenshotSetupStep({
  onNext,
  onSkip,
}: ScreenshotSetupStepProps) {
  const queryClient = useQueryClient()

  // Fetch current settings
  const { data: settings } = useQuery({
    queryKey: ['platform-settings'],
    queryFn: getPlatformSettings,
  })

  // Track if user has changed the value, otherwise use settings value
  const [userEnabled, setUserEnabled] = useState<boolean | null>(null)

  // Use user's choice if they've toggled, otherwise use current settings or default to false
  const enabled =
    userEnabled !== null
      ? userEnabled
      : (settings?.screenshots?.enabled ?? false)

  const updateSettings = useMutation({
    mutationFn: async (screenshotsEnabled: boolean) => {
      return await updatePlatformSettings({
        allow_readonly_external_access:
          settings?.allow_readonly_external_access ?? false,
        dns_provider: settings?.dns_provider ?? {
          provider: 'manual',
          cloudflare_api_key: null,
        },
        external_url: settings?.external_url ?? null,
        letsencrypt: settings?.letsencrypt ?? {
          email: null,
          environment: 'production',
        },
        preview_domain: settings?.preview_domain ?? 'localho.st',
        screenshots: {
          enabled: screenshotsEnabled,
          provider: 'local',
          url: '',
        },
      })
    },
    meta: {
      errorTitle: 'Failed to update screenshot settings',
    },
    onSuccess: () => {
      toast.success(
        enabled
          ? 'Screenshot generation enabled!'
          : 'Screenshot generation disabled'
      )
      queryClient.invalidateQueries({ queryKey: ['getSettings'] })
      queryClient.invalidateQueries({ queryKey: ['platform-settings'] })
      onNext()
    },
  })

  const handleSave = () => {
    updateSettings.mutate(enabled)
  }

  const handleSkip = () => {
    // Don't save anything - just skip to the next step
    // Screenshot settings can be configured later in Settings
    onSkip()
  }

  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <div className="flex justify-center mb-4">
          <div className="flex h-16 w-16 items-center justify-center rounded-full bg-primary/10">
            <Camera className="h-8 w-8 text-primary" />
          </div>
        </div>
        <h2 className="text-2xl font-bold">Screenshot Generation</h2>
        <p className="text-muted-foreground">
          Automatically capture preview images of your deployments
        </p>
      </div>

      <Alert>
        <Info className="h-4 w-4" />
        <AlertDescription>
          <p className="text-sm">
            Screenshot generation creates visual previews of your deployed
            applications. This feature:
          </p>
          <ul className="mt-2 space-y-1 text-sm ml-4">
            <li>• Helps you quickly identify deployments</li>
            <li>• Creates thumbnails for the dashboard</li>
            <li>• Uses a local headless browser</li>
            <li>• Can be configured or disabled later</li>
          </ul>
        </AlertDescription>
      </Alert>

      <div className="flex items-center justify-between p-6 border-2 rounded-lg bg-accent/50">
        <div className="space-y-1">
          <Label htmlFor="screenshot-toggle" className="text-base font-medium">
            Enable Screenshot Generation
          </Label>
          <p className="text-sm text-muted-foreground">
            Use local browser to capture deployment previews
          </p>
        </div>
        <Switch
          id="screenshot-toggle"
          checked={enabled}
          onCheckedChange={setUserEnabled}
        />
      </div>

      {enabled && (
        <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
          <CheckCircle2 className="h-4 w-4 text-blue-600" />
          <AlertDescription>
            <p className="text-sm">
              <strong>Local screenshots enabled.</strong> Make sure you have a
              browser installed (Chrome/Chromium recommended).
            </p>
          </AlertDescription>
        </Alert>
      )}

      <div className="flex justify-between">
        <Button variant="outline" onClick={handleSkip}>
          Skip for Now
        </Button>
        <Button onClick={handleSave} disabled={updateSettings.isPending}>
          {updateSettings.isPending ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              Saving...
            </>
          ) : (
            'Continue'
          )}
        </Button>
      </div>
    </div>
  )
}

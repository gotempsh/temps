import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  useSettings,
  useUpdateSettings,
  type PlatformSettings,
} from '@/hooks/useSettings'
import { AlertCircle, Globe, Image, Link, Loader2, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

type SettingsFormData = Pick<
  PlatformSettings,
  'external_url' | 'preview_domain' | 'screenshots'
>

export function Settings() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()

  const {
    register,
    handleSubmit,
    formState: { isDirty, isSubmitting },
    reset,
    watch,
    setValue,
  } = useForm<SettingsFormData>({
    defaultValues: {
      external_url: '',
      preview_domain: 'localho.st',
      screenshots: {
        enabled: false,
        provider: 'local',
        url: '',
      },
    },
  })

  const screenshots = watch('screenshots')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings' }])
  }, [setBreadcrumbs])

  usePageTitle('Settings')

  useEffect(() => {
    if (settings) {
      reset({
        external_url: settings.external_url || '',
        preview_domain: settings.preview_domain || 'localho.st',
        screenshots: settings.screenshots || {
          enabled: false,
          provider: 'local',
          url: '',
        },
      })
    }
  }, [settings, reset])

  const onSubmit = async (data: SettingsFormData) => {
    try {
      await updateSettings.mutateAsync(data)
      reset(data)
      toast.success('Settings saved successfully')
    } catch (error) {
      console.error('Failed to save settings:', error)
      toast.error('Failed to save settings. Please try again.')
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && isDirty && !isSubmitting) {
      e.preventDefault()
      handleSubmit(onSubmit)()
    }
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin" />
      </div>
    )
  }

  if (error) {
    return (
      <div className="container mx-auto py-8">
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Error</AlertTitle>
          <AlertDescription>
            Failed to load settings. Please try again later.
          </AlertDescription>
        </Alert>
      </div>
    )
  }

  return (
    <div className="w-full px-4 sm:px-6 lg:px-8 py-8" onKeyDown={handleKeyDown}>
      <form onSubmit={handleSubmit(onSubmit)} className="space-y-6">
        <div className="flex justify-between items-center">
          <div>
            <h2 className="text-2xl font-bold tracking-tight">
              Platform Settings
            </h2>
            <p className="text-muted-foreground">
              Configure your platform settings and integrations
            </p>
          </div>
          <Button type="submit" disabled={!isDirty || isSubmitting}>
            {isSubmitting ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                Saving...
              </>
            ) : (
              <>
                <Save className="mr-2 h-4 w-4" />
                Save Changes
              </>
            )}
          </Button>
        </div>

        <Separator />

        <div className="space-y-6">
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Link className="h-5 w-5" />
                External URL
              </CardTitle>
              <CardDescription>
                Set the external URL for your platform
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="space-y-2">
                <Label htmlFor="external-url">External URL</Label>
                <Input
                  id="external-url"
                  type="url"
                  placeholder="https://your-domain.com"
                  {...register('external_url')}
                />
                <p className="text-sm text-muted-foreground">
                  Used for OAuth callbacks, webhooks, and external integrations
                </p>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Globe className="h-5 w-5" />
                Preview Domain
              </CardTitle>
              <CardDescription>
                Configure the domain used for deployment previews
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="space-y-2">
                <Label htmlFor="preview-domain">Preview Domain</Label>
                <Input
                  id="preview-domain"
                  type="text"
                  placeholder="localho.st"
                  {...register('preview_domain')}
                />
                <p className="text-sm text-muted-foreground">
                  Deployments will be accessible at subdomain.
                  {settings?.preview_domain || 'localho.st'}
                </p>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Image className="h-5 w-5" />
                Screenshots
              </CardTitle>
              <CardDescription>
                Configure screenshot generation for deployments
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex items-center justify-between">
                <div className="space-y-0.5">
                  <Label htmlFor="screenshots-enabled">
                    Enable Screenshots
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Generate screenshots of deployments for previews
                  </p>
                </div>
                <Switch
                  id="screenshots-enabled"
                  checked={screenshots?.enabled}
                  onCheckedChange={(checked) =>
                    setValue('screenshots.enabled', checked, {
                      shouldDirty: true,
                    })
                  }
                />
              </div>

              {screenshots?.enabled && (
                <>
                  <div className="space-y-2">
                    <Label htmlFor="screenshot-provider">Provider</Label>
                    <Select
                      value={screenshots?.provider}
                      onValueChange={(value: 'local' | 'external') =>
                        setValue('screenshots.provider', value, {
                          shouldDirty: true,
                        })
                      }
                    >
                      <SelectTrigger id="screenshot-provider">
                        <SelectValue placeholder="Select provider" />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="local">
                          Local Screenshot Service
                        </SelectItem>
                        <SelectItem value="external">
                          External Screenshot API
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </div>

                  {screenshots.provider === 'external' && (
                    <div className="space-y-2">
                      <Label htmlFor="screenshot-url">Screenshot API URL</Label>
                      <Input
                        id="screenshot-url"
                        type="url"
                        placeholder="https://<your-domain>/api/screenshot?url={url}&width=1920&height=1080"
                        {...register('screenshots.url')}
                      />
                      <p className="text-sm text-muted-foreground">
                        Configure your API endpoint with{' '}
                        <code className="px-1 py-0.5 bg-muted rounded text-xs">
                          {'{url}'}
                        </code>{' '}
                        placeholder.
                      </p>
                      <p className="text-sm text-muted-foreground">
                        Example:{' '}
                        <code className="px-1 py-0.5 bg-muted rounded text-xs">
                          https://screenshot-worker.example.com/?url={'{url}'}
                          &width=1920&height=1080
                        </code>
                      </p>
                    </div>
                  )}
                </>
              )}
            </CardContent>
          </Card>
        </div>
      </form>
    </div>
  )
}

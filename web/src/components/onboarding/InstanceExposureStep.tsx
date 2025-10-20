import { Alert, AlertDescription } from '@/components/ui/alert'
import { Globe, Lock, Info, CheckCircle2 } from 'lucide-react'
import { cn } from '@/lib/utils'

interface InstanceExposureStepProps {
  onSelect: (wantsExpose: boolean) => void
  selectedValue: boolean | null
}

export function InstanceExposureStep({
  onSelect,
  selectedValue,
}: InstanceExposureStepProps) {
  return (
    <div className="space-y-6">
      <div className="text-center space-y-2">
        <h2 className="text-2xl font-bold">Welcome to Temps!</h2>
        <p className="text-muted-foreground">
          Let&apos;s set up your deployment platform
        </p>
      </div>

      <Alert>
        <Info className="h-4 w-4" />
        <AlertDescription>
          Exposing your instance allows automatic HTTPS, webhook support, and
          external access for deployments.
        </AlertDescription>
      </Alert>

      <div className="space-y-3">
        <p className="text-sm font-medium">
          Do you want to expose this instance to the internet?
        </p>

        <div className="grid gap-3">
          {/* Expose Publicly Option */}
          <button
            onClick={() => onSelect(true)}
            className={cn(
              'relative p-6 border-2 rounded-lg transition-all text-left group hover:border-primary/50',
              selectedValue === true
                ? 'border-primary bg-primary/5'
                : 'border-border hover:bg-accent/30'
            )}
          >
            <div className="flex items-start gap-4">
              <div
                className={cn(
                  'flex h-12 w-12 items-center justify-center rounded-lg transition-colors',
                  selectedValue === true
                    ? 'bg-primary text-primary-foreground'
                    : 'bg-muted text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary'
                )}
              >
                <Globe className="h-6 w-6" />
              </div>
              <div className="flex-1">
                <div className="flex items-center gap-2 mb-1">
                  <h3 className="font-semibold text-base">
                    Yes, expose publicly
                  </h3>
                  <span className="text-xs font-medium px-2 py-0.5 rounded-full bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400">
                    Recommended
                  </span>
                </div>
                <p className="text-sm text-muted-foreground mb-3">
                  Make your instance accessible from the internet with automatic
                  HTTPS certificates
                </p>
                <div className="space-y-1.5">
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-600" />
                    <span>Automatic HTTPS with Let&apos;s Encrypt</span>
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-600" />
                    <span>Webhook support for CI/CD</span>
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-600" />
                    <span>External access for team members</span>
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-green-600" />
                    <span>Custom domain support</span>
                  </div>
                </div>
              </div>
              {selectedValue === true && (
                <CheckCircle2 className="h-5 w-5 text-primary absolute top-4 right-4" />
              )}
            </div>
          </button>

          {/* Keep Local Option */}
          <button
            onClick={() => onSelect(false)}
            className={cn(
              'relative p-6 border-2 rounded-lg transition-all text-left group hover:border-primary/50',
              selectedValue === false
                ? 'border-primary bg-primary/5'
                : 'border-border hover:bg-accent/30'
            )}
          >
            <div className="flex items-start gap-4">
              <div
                className={cn(
                  'flex h-12 w-12 items-center justify-center rounded-lg transition-colors',
                  selectedValue === false
                    ? 'bg-primary text-primary-foreground'
                    : 'bg-muted text-muted-foreground group-hover:bg-primary/10 group-hover:text-primary'
                )}
              >
                <Lock className="h-6 w-6" />
              </div>
              <div className="flex-1">
                <h3 className="font-semibold text-base mb-1">
                  No, keep local only
                </h3>
                <p className="text-sm text-muted-foreground mb-3">
                  Run Temps on your local network without internet exposure
                </p>
                <div className="space-y-1.5">
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-muted-foreground" />
                    <span>Development and testing only</span>
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-muted-foreground" />
                    <span>No external webhooks</span>
                  </div>
                  <div className="flex items-center gap-2 text-xs text-muted-foreground">
                    <CheckCircle2 className="h-3.5 w-3.5 text-muted-foreground" />
                    <span>Local network access only</span>
                  </div>
                </div>
              </div>
              {selectedValue === false && (
                <CheckCircle2 className="h-5 w-5 text-primary absolute top-4 right-4" />
              )}
            </div>
          </button>
        </div>
      </div>

      {selectedValue === false && (
        <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
          <Info className="h-4 w-4 text-orange-600" />
          <AlertDescription className="text-sm">
            <strong>Note:</strong> Running in local mode will limit some
            features. You can always expose your instance later from settings.
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

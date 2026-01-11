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
import { Slider } from '@/components/ui/slider'
import { Switch } from '@/components/ui/switch'
import { HardDrive, AlertTriangle } from 'lucide-react'
import type { Control, UseFormRegister, UseFormSetValue } from 'react-hook-form'
import type { DiskSpaceAlertSettings } from '@/api/platformSettings'

interface MonitoringSettingsProps {
  control: Control<any>
  register: UseFormRegister<any>
  setValue: UseFormSetValue<any>
  diskSpaceAlert: DiskSpaceAlertSettings | undefined
}

// Check interval options in seconds
const CHECK_INTERVALS = [
  { value: 60, label: '1 minute' },
  { value: 300, label: '5 minutes' },
  { value: 600, label: '10 minutes' },
  { value: 1800, label: '30 minutes' },
  { value: 3600, label: '1 hour' },
]

export function MonitoringSettings({
  setValue,
  diskSpaceAlert,
}: MonitoringSettingsProps) {
  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <HardDrive className="h-5 w-5" />
            Disk Space Alerts
          </CardTitle>
          <CardDescription>
            Configure alerts for low disk space on your server
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-6">
          {/* Enable/Disable Toggle */}
          <div className="flex items-center justify-between">
            <div className="space-y-0.5">
              <Label htmlFor="disk-space-enabled">Enable Disk Space Alerts</Label>
              <p className="text-sm text-muted-foreground">
                Receive notifications when disk usage exceeds the threshold
              </p>
            </div>
            <Switch
              id="disk-space-enabled"
              checked={diskSpaceAlert?.enabled}
              onCheckedChange={(checked) =>
                setValue('disk_space_alert.enabled', checked, {
                  shouldDirty: true,
                })
              }
            />
          </div>

          {diskSpaceAlert?.enabled && (
            <>
              {/* Threshold Slider */}
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <Label htmlFor="threshold-slider">Alert Threshold</Label>
                  <span className="text-sm font-medium tabular-nums">
                    {diskSpaceAlert?.threshold_percent || 80}%
                  </span>
                </div>
                <div className="flex items-center gap-4">
                  <Slider
                    id="threshold-slider"
                    value={[diskSpaceAlert?.threshold_percent || 80]}
                    min={50}
                    max={99}
                    step={1}
                    onValueChange={([value]: number[]) =>
                      setValue('disk_space_alert.threshold_percent', value, {
                        shouldDirty: true,
                      })
                    }
                    className="flex-1"
                  />
                </div>
                <p className="text-sm text-muted-foreground">
                  Alert when disk usage reaches this percentage. Recommended: 80%
                </p>

                {/* Threshold Warning */}
                {diskSpaceAlert?.threshold_percent &&
                  diskSpaceAlert.threshold_percent >= 90 && (
                    <div className="flex items-center gap-2 p-3 rounded-md bg-yellow-50 dark:bg-yellow-900/20 text-yellow-800 dark:text-yellow-200">
                      <AlertTriangle className="h-4 w-4 flex-shrink-0" />
                      <p className="text-sm">
                        Setting a high threshold (90%+) may not give you enough
                        time to free up space before issues occur.
                      </p>
                    </div>
                  )}
              </div>

              {/* Check Interval */}
              <div className="space-y-2">
                <Label htmlFor="check-interval">Check Interval</Label>
                <Select
                  value={String(diskSpaceAlert?.check_interval_seconds || 300)}
                  onValueChange={(value) =>
                    setValue(
                      'disk_space_alert.check_interval_seconds',
                      parseInt(value, 10),
                      { shouldDirty: true }
                    )
                  }
                >
                  <SelectTrigger id="check-interval">
                    <SelectValue placeholder="Select interval" />
                  </SelectTrigger>
                  <SelectContent>
                    {CHECK_INTERVALS.map((interval) => (
                      <SelectItem
                        key={interval.value}
                        value={String(interval.value)}
                      >
                        {interval.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <p className="text-sm text-muted-foreground">
                  How often to check disk space usage
                </p>
              </div>

              {/* Monitor Path (Optional) */}
              <div className="space-y-2">
                <Label htmlFor="monitor-path">Monitor Path (Optional)</Label>
                <Input
                  id="monitor-path"
                  type="text"
                  placeholder="Leave empty to monitor data directory"
                  value={diskSpaceAlert?.monitor_path || ''}
                  onChange={(e) =>
                    setValue(
                      'disk_space_alert.monitor_path',
                      e.target.value || null,
                      { shouldDirty: true }
                    )
                  }
                />
                <p className="text-sm text-muted-foreground">
                  Specify a custom path to monitor, or leave empty to monitor
                  the data directory
                </p>
              </div>
            </>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

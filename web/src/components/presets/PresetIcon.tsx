import { Boxes } from 'lucide-react'
import { useState } from 'react'
import { cn } from '@/lib/utils'
import {
  presetIconNeedsDarkInvert,
  presetIconPath,
} from '@/components/presets/preset-icon-paths'

export function PresetIcon({
  preset,
  label,
  className,
  imageClassName,
}: {
  preset: string
  label?: string
  className?: string
  imageClassName?: string
}) {
  const [failedPreset, setFailedPreset] = useState<string | null>(null)
  const iconPath = presetIconPath(preset)
  const canShowIcon = !!iconPath && failedPreset !== preset

  return (
    <div
      className={cn(
        'flex items-center justify-center overflow-hidden rounded-xl border bg-background text-muted-foreground',
        className
      )}
    >
      {canShowIcon ? (
        <img
          src={iconPath}
          alt={`${label || preset} logo`}
          className={cn(
            'size-full object-contain',
            presetIconNeedsDarkInvert(preset) && 'dark:invert',
            imageClassName
          )}
          onError={() => setFailedPreset(preset)}
        />
      ) : (
        <Boxes className="size-1/2" strokeWidth={1.5} />
      )}
    </div>
  )
}

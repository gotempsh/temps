import { useState } from 'react'
import { GitBranch } from 'lucide-react'
import { cn } from '@/lib/utils'
import { presetIconPath } from '@/components/presets/preset-icon-paths'

interface TemplateImageProps {
  imageUrl?: string | null
  preset: string
  alt: string
  className?: string
  imgClassName?: string
  fallbackClassName?: string
}

/**
 * Renders a template's image with a graceful fallback chain:
 *   1. Remote `imageUrl` (e.g. preview screenshot)
 *   2. Local preset icon (e.g. `/presets/nextjs.svg`)
 *   3. Generic GitBranch icon
 *
 * Failures at each step are detected via `onError` and trigger the next fallback.
 */
export function TemplateImage({
  imageUrl,
  preset,
  alt,
  className,
  imgClassName,
  fallbackClassName,
}: TemplateImageProps) {
  const [imageFailed, setImageFailed] = useState(false)
  const [presetIconFailed, setPresetIconFailed] = useState(false)

  const showImage = !!imageUrl && !imageFailed
  const presetIcon = presetIconPath(preset)
  const showPresetIcon = !showImage && !!presetIcon && !presetIconFailed

  return (
    <div
      className={cn(
        'flex items-center justify-center overflow-hidden rounded-md bg-muted text-muted-foreground',
        className
      )}
    >
      {showImage ? (
        <img
          src={imageUrl!}
          alt={alt}
          className={cn('object-contain', imgClassName)}
          onError={() => setImageFailed(true)}
        />
      ) : showPresetIcon ? (
        <img
          src={presetIcon}
          alt={preset}
          className={cn('object-contain', imgClassName)}
          onError={() => setPresetIconFailed(true)}
        />
      ) : (
        <GitBranch className={cn('h-5 w-5', fallbackClassName)} />
      )}
    </div>
  )
}

import { useState } from 'react'
import { GitBranch } from 'lucide-react'
import { cn } from '@/lib/utils'

const PRESET_ICONS: Record<string, string> = {
  nextjs: '/presets/nextjs.svg',
  fastapi: '/presets/fastapi.svg',
  django: '/presets/django.svg',
  remix: '/presets/remix.svg',
  nuxt: '/presets/nuxt.svg',
  astro: '/presets/astro.svg',
  rust: '/presets/rust.svg',
  go: '/presets/go.svg',
  nixpacks: '/presets/nixpacks.svg',
  vite: '/presets/vite.svg',
  react: '/presets/react.svg',
  vue: '/presets/vue.svg',
  angular: '/presets/angular.svg',
  flask: '/presets/flask.svg',
  laravel: '/presets/laravel.svg',
  rails: '/presets/rails.svg',
  nodejs: '/presets/nodejs.svg',
  sveltekit: '/presets/sveltekit.svg',
  solidstart: '/presets/solidstart.svg',
  docusaurus: '/presets/docusaurus.svg',
  dockerfile: '/presets/dockerfile.svg',
  docker: '/presets/docker.svg',
  static: '/presets/static.svg',
  rsbuild: '/presets/rsbuild.svg',
}

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
  const presetIcon = PRESET_ICONS[preset.toLowerCase()]
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

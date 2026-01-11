import { usePresets } from '@/contexts/PresetContext'

export default function FrameworkIcon({
  preset,
  className,
}: {
  preset: string
  className?: string
}) {
  const { getPresetBySlug } = usePresets()
  const presetInfo = getPresetBySlug(preset)

  // Use preset icon from server if available, otherwise use fallback
  const iconUrl = presetInfo?.icon_url || '/presets/custom.svg'
  const altText = presetInfo?.label || preset

  return (
    <img
      src={iconUrl}
      alt={altText}
      className={`${className} dark:invert`}
      style={{ objectFit: 'contain' }}
      onError={(e) => {
        // Fallback to custom icon if loading fails
        e.currentTarget.src = '/presets/custom.svg'
      }}
    />
  )
}

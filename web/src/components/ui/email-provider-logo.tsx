import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import { cn } from '@/lib/utils'
import { Mail } from 'lucide-react'

export type EmailProviderType = 'ses' | 'scaleway' | 'smtp'

interface ProviderConfig {
  icon: React.ComponentType<{ className?: string; width?: number; height?: number }>
  label: string
  color: string
}

const emailProviderConfig: Record<EmailProviderType, ProviderConfig> = {
  ses: {
    icon: AWSIcon,
    label: 'AWS SES',
    color: 'text-[#FF9900]',
  },
  scaleway: {
    icon: ScalewayIcon,
    label: 'Scaleway',
    color: 'text-[#4F0599]',
  },
  smtp: {
    icon: Mail,
    label: 'SMTP',
    color: 'text-slate-600 dark:text-slate-300',
  },
}

interface EmailProviderLogoProps extends React.HTMLAttributes<HTMLDivElement> {
  provider: EmailProviderType
  size?: number
  showLabel?: boolean
}

export function EmailProviderLogo({
  provider,
  size = 24,
  showLabel = false,
  className,
  ...props
}: EmailProviderLogoProps) {
  const config = emailProviderConfig[provider]

  if (!config) return null

  const Icon = config.icon

  return (
    <div className={cn('flex items-center gap-2', className)} {...props}>
      <Icon className={config.color} width={size} height={size} />
      {showLabel && (
        <span className="font-medium">{config.label}</span>
      )}
    </div>
  )
}

export function getEmailProviderLabel(provider: EmailProviderType): string {
  return emailProviderConfig[provider]?.label || provider.toUpperCase()
}

export function getEmailProviderColor(provider: EmailProviderType): string {
  return emailProviderConfig[provider]?.color || ''
}

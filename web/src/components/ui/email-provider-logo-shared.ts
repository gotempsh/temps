// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AWSIcon } from '@/components/icons/AWSIcon'
import { ScalewayIcon } from '@/components/icons/ScalewayIcon'
import { Mail } from 'lucide-react'

export type EmailProviderType = 'ses' | 'scaleway' | 'smtp'

export interface ProviderConfig {
  icon: React.ComponentType<{
    className?: string
    width?: number
    height?: number
  }>
  label: string
  color: string
}

export const emailProviderConfig: Record<EmailProviderType, ProviderConfig> = {
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

export interface EmailProviderLogoProps extends React.HTMLAttributes<HTMLDivElement> {
  provider: EmailProviderType
  size?: number
  showLabel?: boolean
}

export function getEmailProviderLabel(provider: EmailProviderType): string {
  return emailProviderConfig[provider]?.label || provider.toUpperCase()
}

export function getEmailProviderColor(provider: EmailProviderType): string {
  return emailProviderConfig[provider]?.color || ''
}

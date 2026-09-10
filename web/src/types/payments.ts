// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface PaymentProviderTemplate {
  id: string
  name: string
  description: string
  icon: React.ComponentType<{ className?: string }>
  isComingSoon?: boolean
  fields?: {
    id: string
    label: string
    type: 'text' | 'password'
    placeholder: string
  }[]
}

export interface PaymentProviderInstance extends PaymentProviderTemplate {
  instanceId: string
  enabled: boolean
  name: string
}

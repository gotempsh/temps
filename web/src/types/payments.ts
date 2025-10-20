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

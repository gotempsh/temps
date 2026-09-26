// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { AI_PROVIDERS, AiProviderIcon } from '@/lib/ai-providers'
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/ui/select'

// Telemetry may include providers that the gateway does not configure itself.
const GENAI_PROVIDERS = [
  ...AI_PROVIDERS,
  { id: 'mistral', name: 'Mistral' },
  { id: 'deepseek', name: 'DeepSeek' },
]

export function AiProviderLabel({
  provider,
  name,
}: {
  provider: string
  name?: string
}) {
  return (
    <span className="inline-flex min-w-0 items-center gap-2">
      <AiProviderIcon
        provider={provider}
        tinted={false}
        size={16}
        className="shrink-0"
      />
      <span className="truncate">
        {name ??
          GENAI_PROVIDERS.find((item) => item.id === provider)?.name ??
          provider}
      </span>
    </span>
  )
}

export function AiProviderSelect({
  value,
  onValueChange,
  providers = GENAI_PROVIDERS,
  className = 'w-full sm:w-[190px]',
}: {
  value: string
  onValueChange: (value: string) => void
  providers?: readonly { id: string; name: string }[]
  className?: string
}) {
  return (
    <Select value={value} onValueChange={onValueChange}>
      <SelectTrigger aria-label="AI provider" className={className}>
        <SelectValue placeholder="All providers" />
      </SelectTrigger>
      <SelectContent>
        <SelectItem value="all">All providers</SelectItem>
        {providers.map((provider) => (
          <SelectItem key={provider.id} value={provider.id}>
            <AiProviderLabel provider={provider.id} name={provider.name} />
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

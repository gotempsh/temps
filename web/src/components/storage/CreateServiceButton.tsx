// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { KbdBadge } from '@/components/ui/kbd-badge'
import { Plus, ChevronDown, Loader2 } from 'lucide-react'
import { ProviderMetadata } from '@/api/client/types.gen'
import { getProvidersMetadataOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from 'react-router'
import { serviceCreateHref } from '@/lib/service-project-link'

export function CreateServiceButton({
  open,
  onOpenChange,
  projectId = null,
  label = 'Create Service',
}: {
  projectId?: number | null
  label?: string
  onSuccess?: () => void
  open?: boolean
  onOpenChange?: (open: boolean) => void
}) {
  const navigate = useNavigate()

  const {
    data: providers,
    isLoading,
    isError,
    refetch,
  } = useQuery({
    ...getProvidersMetadataOptions(),
  })

  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <Button className="gap-2" disabled={isLoading}>
          {isLoading ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Plus className="h-4 w-4" />
          )}
          {label}
          <KbdBadge keys="N" className="ml-1" />
          <ChevronDown className="h-4 w-4 ml-1" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-[260px]">
        {isError && (
          <DropdownMenuItem
            onSelect={(event) => {
              event.preventDefault()
              void refetch()
            }}
          >
            Could not load database types. Retry
          </DropdownMenuItem>
        )}
        {!isError && !isLoading && !providers?.length && (
          <p className="p-3 text-sm text-muted-foreground">
            No database types are available.
          </p>
        )}
        {providers?.map((provider: ProviderMetadata) => (
          <DropdownMenuItem
            key={provider.service_type}
            onClick={() => {
              navigate(serviceCreateHref(provider.service_type, projectId))
            }}
            className="flex items-start gap-3 py-3 cursor-pointer"
          >
            <div
              className="flex items-center justify-center rounded-md p-1.5"
              style={{ backgroundColor: provider.color }}
            >
              <img
                src={provider.icon_url}
                alt={`${provider.display_name} logo`}
                width={20}
                height={20}
                className="rounded-sm brightness-0 invert"
              />
            </div>
            <div className="flex flex-col flex-1">
              <span className="font-medium">{provider.display_name}</span>
              <span className="text-xs text-muted-foreground">
                {provider.description}
              </span>
            </div>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

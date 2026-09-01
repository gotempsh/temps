// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { RepositoryList } from './RepositoryList'
import { RepositoryResponse } from '@/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Check, X } from 'lucide-react'
import { cn } from '@/lib/utils'
import { RepositoryAvatar } from './RepositoryAvatar'

interface RepositorySelectorProps {
  connectionId: number
  onSelect: (repo: RepositoryResponse | null) => void
  selectedRepository?: RepositoryResponse | null
  title?: string
  description?: string
  className?: string
  showAsCard?: boolean
  compactMode?: boolean
  itemsPerPage?: number
  providerType?: string | null
}

export function RepositorySelector({
  connectionId,
  onSelect,
  selectedRepository,
  title = 'Select Repository',
  description,
  className,
  showAsCard = true,
  compactMode = false,
  itemsPerPage = 12,
  providerType,
}: RepositorySelectorProps) {
  const [forceSelecting, setForceSelecting] = useState(false)
  const isSelecting = forceSelecting || !selectedRepository

  const handleRepositorySelect = (repo: RepositoryResponse) => {
    onSelect(repo)
    setForceSelecting(false)
  }

  const handleClearSelection = () => {
    onSelect(null)
    setForceSelecting(true)
  }

  const handleChangeSelection = () => {
    onSelect(null)
    setForceSelecting(true)
  }

  if (isSelecting) {
    if (showAsCard) {
      return (
        <Card className={className}>
          <CardHeader className="pb-3">
            <CardTitle className="text-lg">{title}</CardTitle>
            {description && (
              <p className="text-sm text-muted-foreground">{description}</p>
            )}
          </CardHeader>
          <CardContent className="p-4 pt-0">
            <RepositoryList
              connectionId={connectionId}
              onRepositorySelect={handleRepositorySelect}
              showSelection={false}
              itemsPerPage={itemsPerPage}
              showHeader={true}
              compactMode={compactMode}
              providerType={providerType}
            />
          </CardContent>
        </Card>
      )
    }

    return (
      <div className={className}>
        <RepositoryList
          connectionId={connectionId}
          onRepositorySelect={handleRepositorySelect}
          showSelection={false}
          itemsPerPage={itemsPerPage}
          showHeader={true}
          compactMode={compactMode}
          providerType={providerType}
        />
      </div>
    )
  }

  // Show selected repository
  return (
    <div className={cn('space-y-3', className)}>
      {(title || description) && (
        <div>
          {title && <h3 className="text-sm font-medium">{title}</h3>}
          {description && (
            <p className="text-sm text-muted-foreground mt-1">{description}</p>
          )}
        </div>
      )}

      <div className="flex items-center justify-between rounded-lg border border-primary bg-primary/5 p-3">
        <div className="flex min-w-0 items-center gap-3">
          {selectedRepository && (
            <RepositoryAvatar
              repository={selectedRepository}
              providerType={providerType}
            />
          )}
          <div className="min-w-0">
            <div className="font-medium">
              {selectedRepository?.owner}/{selectedRepository?.name}
            </div>
            {selectedRepository?.description && (
              <div className="text-sm text-muted-foreground line-clamp-1">
                {selectedRepository.description}
              </div>
            )}
          </div>
          {selectedRepository?.private && (
            <Badge variant="secondary">Private</Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Badge variant="outline" className="text-xs">
            <Check className="h-3 w-3 mr-1" />
            Selected
          </Badge>
          <Button variant="ghost" size="sm" onClick={handleChangeSelection}>
            Change
          </Button>
          <Button variant="ghost" size="icon" onClick={handleClearSelection}>
            <X className="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  )
}

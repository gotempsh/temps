import { useState } from 'react'
import { RepositoryList } from './RepositoryList'
import { RepositoryResponse } from '@/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { GitBranch, Check, X } from 'lucide-react'
import { cn } from '@/lib/utils'

interface RepositorySelectorProps {
  connectionId: number
  onSelect: (repo: RepositoryResponse | null) => void
  selectedRepository?: RepositoryResponse | null
  title?: string
  description?: string
  className?: string
  showAsCard?: boolean
}

export function RepositorySelector({
  connectionId,
  onSelect,
  selectedRepository,
  title = 'Select Repository',
  description,
  className,
  showAsCard = true,
}: RepositorySelectorProps) {
  const [isSelecting, setIsSelecting] = useState(!selectedRepository)

  const handleRepositorySelect = (repo: RepositoryResponse) => {
    onSelect(repo)
    setIsSelecting(false)
  }

  const handleClearSelection = () => {
    onSelect(null)
    setIsSelecting(true)
  }

  const handleChangeSelection = () => {
    setIsSelecting(true)
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
              itemsPerPage={12}
              showHeader={true}
              compactMode={false}
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
          itemsPerPage={12}
          showHeader={true}
          compactMode={false}
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

      <div className="flex items-center justify-between p-4 border rounded-lg bg-primary/5 border-primary">
        <div className="flex items-center gap-3">
          <GitBranch className="h-5 w-5 text-muted-foreground" />
          <div>
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

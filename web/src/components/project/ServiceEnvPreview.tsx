import { useCallback, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getServicePreviewEnvironmentVariablesMaskedOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Collapsible, CollapsibleContent } from '@/components/ui/collapsible'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { ChevronDown, ChevronRight, Eye, EyeOff, Loader2 } from 'lucide-react'
import { EnvVariablesDisplay } from '@/components/ui/env-variables-display'

interface ServiceEnvPreviewProps {
  serviceId: number
  serviceName: string
  serviceType: string
}

export function ServiceEnvPreview({
  serviceId,
  serviceName,
  serviceType,
}: ServiceEnvPreviewProps) {
  const [isOpen, setIsOpen] = useState(false)
  const [showPreview, setShowPreview] = useState(false)

  const {
    data: envVars,
    isLoading,
    error,
  } = useQuery({
    ...getServicePreviewEnvironmentVariablesMaskedOptions({
      path: { id: serviceId },
    }),
    enabled: isOpen, // Auto-fetch when expanded
    staleTime: 5 * 60 * 1000, // Cache for 5 minutes
  })

  const handlePreviewToggle = useCallback(() => {
    if (!showPreview && !isOpen) {
      setIsOpen(true)
    }
    setShowPreview(!showPreview)
  }, [showPreview, isOpen])

  // Auto-load preview when expanding
  const handleToggleExpand = () => {
    setIsOpen(!isOpen)
    if (!isOpen) {
      setShowPreview(true) // Auto-show preview when expanding
    }
  }

  return (
    <Collapsible open={isOpen} onOpenChange={setIsOpen}>
      <Card className="border-dashed border-muted-foreground/30">
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="h-6 w-6 p-0"
                onClick={handleToggleExpand}
              >
                {isOpen ? (
                  <ChevronDown className="h-3 w-3" />
                ) : (
                  <ChevronRight className="h-3 w-3" />
                )}
              </Button>
              <CardTitle className="text-sm">{serviceName}</CardTitle>
              <Badge variant="outline" className="text-xs">
                {serviceType}
              </Badge>
            </div>

            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={handlePreviewToggle}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              {showPreview ? (
                <>
                  <EyeOff className="h-3 w-3 mr-1" />
                  Hide Variables
                </>
              ) : (
                <>
                  <Eye className="h-3 w-3 mr-1" />
                  Preview Variables
                </>
              )}
            </Button>
          </div>

          {!isOpen && (
            <CardDescription className="text-xs">
              Click to see available environment variables for this service
            </CardDescription>
          )}
        </CardHeader>

        <CollapsibleContent>
          <CardContent className="pt-0">
            {!showPreview && (
              <div className="text-center py-4">
                <p className="text-xs text-muted-foreground">
                  Click &quot;Preview Variables&quot; to see what environment
                  variables will be available
                </p>
              </div>
            )}

            {isLoading && (
              <div className="flex items-center justify-center py-4">
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
                <span className="text-xs text-muted-foreground">
                  Loading environment variables...
                </span>
              </div>
            )}

            {error && (
              <div className="text-center py-4">
                <p className="text-xs text-destructive">
                  Failed to load environment variables
                </p>
              </div>
            )}

            {envVars && showPreview && (
              <>
                <EnvVariablesDisplay
                  variables={envVars}
                  showCopy={true}
                  showMaskToggle={true}
                  defaultMasked={true}
                  maxHeight="10rem"
                />
                <p className="text-xs text-muted-foreground text-center mt-3">
                  These variables will be automatically available in your
                  deployed application
                </p>
              </>
            )}
          </CardContent>
        </CollapsibleContent>
      </Card>
    </Collapsible>
  )
}

import { AlertCircle, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'

interface CompactErrorFallbackProps {
  error: Error
  resetError: () => void
  componentName?: string
  minimal?: boolean
}

/**
 * Compact error fallback for sidebar/header components
 * Displays a minimal error UI that doesn't take up much space
 */
export function CompactErrorFallback({
  error,
  resetError,
  componentName = 'Component',
  minimal = false,
}: CompactErrorFallbackProps) {
  if (minimal) {
    // Ultra-minimal version for very constrained spaces (e.g., header)
    return (
      <div className="flex items-center gap-2 px-4 py-2 bg-destructive/10 border-l-4 border-destructive">
        <AlertCircle className="h-4 w-4 text-destructive flex-shrink-0" />
        <span className="text-sm text-destructive font-medium flex-1 truncate">
          {componentName} error
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={resetError}
          className="h-7 px-2 text-xs hover:bg-destructive/20"
        >
          <RefreshCw className="h-3 w-3 mr-1" />
          Retry
        </Button>
      </div>
    )
  }

  // Regular compact version for sidebar
  return (
    <Alert variant="destructive" className="m-4">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle className="text-sm">{componentName} Error</AlertTitle>
      <AlertDescription className="text-xs mt-2 space-y-2">
        <p className="line-clamp-2">{error.message}</p>
        <Button
          variant="outline"
          size="sm"
          onClick={resetError}
          className="h-7 text-xs w-full"
        >
          <RefreshCw className="h-3 w-3 mr-1" />
          Try Again
        </Button>
      </AlertDescription>
    </Alert>
  )
}

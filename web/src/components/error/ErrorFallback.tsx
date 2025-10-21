import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { AlertTriangle, RefreshCw, Home } from 'lucide-react'
import { useNavigate } from 'react-router-dom'

interface ErrorFallbackProps {
  error: Error
  errorInfo?: React.ErrorInfo
  resetError: () => void
  showDetails?: boolean
}

/**
 * ErrorFallback component that displays a user-friendly error message
 * with options to retry or navigate back to the dashboard.
 *
 * This component is designed to be used as the fallback UI for ErrorBoundary.
 */
export function ErrorFallback({
  error,
  errorInfo,
  resetError,
  showDetails = process.env.NODE_ENV === 'development',
}: ErrorFallbackProps) {
  const navigate = useNavigate()

  const handleGoHome = () => {
    resetError()
    navigate('/dashboard')
  }

  return (
    <div className="flex min-h-[400px] flex-col items-center justify-center p-4">
      <Card className="w-full max-w-2xl mx-auto">
        <CardHeader>
          <div className="flex items-center gap-3">
            <AlertTriangle className="h-6 w-6 text-destructive" />
            <div>
              <CardTitle>Something went wrong</CardTitle>
              <CardDescription>
                An unexpected error occurred while rendering this page
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <Alert variant="destructive">
            <AlertTitle>Error Details</AlertTitle>
            <AlertDescription>
              <span className="font-mono text-xs break-all">
                {error.message || 'An unknown error occurred'}
              </span>
            </AlertDescription>
          </Alert>

          {showDetails && errorInfo && (
            <details className="rounded-md border p-4 space-y-2">
              <summary className="cursor-pointer font-medium text-sm">
                Technical Details (Development Only)
              </summary>
              <div className="mt-3 space-y-3">
                <div>
                  <p className="text-xs font-medium text-muted-foreground mb-1">
                    Error Stack:
                  </p>
                  <pre className="text-xs bg-muted p-3 rounded overflow-auto max-h-40">
                    {error.stack}
                  </pre>
                </div>
                {errorInfo.componentStack && (
                  <div>
                    <p className="text-xs font-medium text-muted-foreground mb-1">
                      Component Stack:
                    </p>
                    <pre className="text-xs bg-muted p-3 rounded overflow-auto max-h-40">
                      {errorInfo.componentStack}
                    </pre>
                  </div>
                )}
              </div>
            </details>
          )}

          <div className="flex flex-col sm:flex-row gap-3 pt-2">
            <Button onClick={resetError} className="flex-1 sm:flex-none">
              <RefreshCw className="mr-2 h-4 w-4" />
              Try Again
            </Button>
            <Button
              onClick={handleGoHome}
              variant="outline"
              className="flex-1 sm:flex-none"
            >
              <Home className="mr-2 h-4 w-4" />
              Back to Dashboard
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

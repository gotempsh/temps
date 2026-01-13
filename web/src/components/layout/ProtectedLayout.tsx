import { useAuth } from '@/contexts/AuthContext'
import { Login } from '@/pages/Login'
import { AlertCircle, RefreshCw, ServerCrash } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { DemoBanner } from './DemoBanner'

export const ProtectedLayout = ({
  children,
}: {
  children: React.ReactNode
}) => {
  const { user, isLoading, error, refetch, isDemoMode } = useAuth()

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-screen">
        <div className="text-center space-y-2">
          <div className="h-8 w-8 animate-spin rounded-full border-4 border-primary border-t-transparent mx-auto" />
          <p className="text-sm text-muted-foreground">Loading...</p>
        </div>
      </div>
    )
  }

  // Handle errors
  if (error) {
    const errorObj = error as any
    const errorTitle = errorObj?.title
    const errorDetail = errorObj?.detail
    const errorMessage =
      error?.message || errorDetail || 'An unexpected error occurred'

    // 401 - Unauthorized: Show login page (user not authenticated)
    // Check for authentication-related errors
    if (
      errorTitle === 'Authentication Required' ||
      errorTitle === 'Unauthorized'
    ) {
      return <Login />
    }

    // 504 Gateway Timeout or connection errors: Show error with retry
    if (
      errorTitle === 'Gateway Timeout' ||
      errorMessage.includes('Failed to fetch') ||
      errorMessage.includes('Network') ||
      errorObj?.code === 'ECONNREFUSED'
    ) {
      return (
        <div className="flex items-center justify-center min-h-screen p-4">
          <div className="max-w-md w-full space-y-4">
            <Alert variant="destructive">
              <ServerCrash className="h-5 w-5" />
              <AlertTitle className="text-lg font-semibold">
                Cannot Connect to Server
              </AlertTitle>
              <AlertDescription className="mt-2 space-y-2">
                <p className="text-sm">
                  {errorTitle === 'Gateway Timeout'
                    ? 'The server is taking too long to respond. This might be a temporary issue.'
                    : 'Unable to connect to the server. Please check your connection and try again.'}
                </p>
                {errorMessage && (
                  <p className="text-xs text-muted-foreground mt-2 font-mono">
                    {errorMessage}
                  </p>
                )}
              </AlertDescription>
            </Alert>
            <Button
              onClick={() => refetch()}
              className="w-full"
              variant="default"
            >
              <RefreshCw className="mr-2 h-4 w-4" />
              Retry Connection
            </Button>
          </div>
        </div>
      )
    }

    // Other errors (non-401, non-504): Show generic error with retry
    // This handles unexpected server errors (500, 503, etc.)
    return (
      <div className="flex items-center justify-center min-h-screen p-4">
        <div className="max-w-md w-full space-y-4">
          <Alert variant="destructive">
            <AlertCircle className="h-5 w-5" />
            <AlertTitle className="text-lg font-semibold">
              Server Error
            </AlertTitle>
            <AlertDescription className="mt-2 space-y-2">
              <p className="text-sm">
                The server encountered an error. Please try again later.
              </p>
              {errorMessage && (
                <p className="text-xs text-muted-foreground mt-2 font-mono">
                  {errorMessage}
                </p>
              )}
            </AlertDescription>
          </Alert>
          <Button
            onClick={() => refetch()}
            className="w-full"
            variant="default"
          >
            <RefreshCw className="mr-2 h-4 w-4" />
            Retry
          </Button>
        </div>
      </div>
    )
  }

  // No error but no user: show login page (normal flow)
  if (!user) {
    return <Login />
  }

  return (
    <>
      {isDemoMode && <DemoBanner showExitButton />}
      {children}
    </>
  )
}

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { useState } from 'react'

/**
 * Test component to verify ErrorBoundary functionality.
 * This component is only for development/testing purposes.
 *
 * Usage: Import and add to a route to test error boundaries
 * Example: <Route path="/test-error" element={<TestErrorBoundary />} />
 */
export function TestErrorBoundary() {
  const [shouldThrow, setShouldThrow] = useState(false)

  if (shouldThrow) {
    throw new Error('Test error thrown intentionally to verify ErrorBoundary')
  }

  return (
    <div className="container max-w-2xl mx-auto py-8">
      <Card>
        <CardHeader>
          <CardTitle>Error Boundary Test</CardTitle>
          <CardDescription>
            Test the error boundary by clicking the button below. This will throw an error
            that should be caught by the ErrorBoundary component, keeping the sidebar and
            header functional.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="rounded-lg bg-muted p-4">
            <h3 className="font-medium mb-2">What to expect:</h3>
            <ul className="list-disc list-inside space-y-1 text-sm text-muted-foreground">
              <li>The sidebar and header will remain functional</li>
              <li>Only the page content will show the error fallback UI</li>
              <li>You can navigate to other pages using the sidebar</li>
              <li>The error fallback will have "Try Again" and "Back to Dashboard" buttons</li>
            </ul>
          </div>

          <Button
            onClick={() => setShouldThrow(true)}
            variant="destructive"
            className="w-full"
          >
            Throw Test Error
          </Button>
        </CardContent>
      </Card>
    </div>
  )
}

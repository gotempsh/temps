import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { useState } from 'react'

/**
 * Test component to demonstrate isolated error boundaries
 *
 * This component allows you to test how errors in different sections
 * (Sidebar, Header, Page Content) are isolated from each other.
 *
 * Usage:
 * 1. Add route in App.tsx: <Route path="/test-isolated-errors" element={<TestIsolatedErrors />} />
 * 2. Navigate to /test-isolated-errors
 * 3. Click buttons to trigger errors in different sections
 *
 * Expected behavior:
 * - Clicking "Crash This Page" should show error UI only for page content
 * - Sidebar and Header should remain functional
 * - You should still be able to navigate using the sidebar
 */
export function TestIsolatedErrors() {
  const [shouldCrash, setShouldCrash] = useState(false)

  if (shouldCrash) {
    // Intentionally throw an error to test error boundary
    throw new Error('Test page crash - This is intentional for testing!')
  }

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <div>
        <h1 className="text-3xl font-bold">Isolated Error Boundary Test</h1>
        <p className="text-muted-foreground mt-2">
          Test how error boundaries isolate failures between different sections
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Test Page Content Crash</CardTitle>
          <CardDescription>
            This button will crash the page content only. The sidebar and header
            should remain functional.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Button
            variant="destructive"
            onClick={() => setShouldCrash(true)}
          >
            Crash This Page
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>What to Expect</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <h3 className="font-semibold mb-2">✅ When Page Crashes:</h3>
            <ul className="list-disc list-inside space-y-1 text-sm text-muted-foreground">
              <li>Page content shows error UI with "Try Again" button</li>
              <li>Sidebar remains functional and interactive</li>
              <li>Header remains functional and interactive</li>
              <li>You can navigate to other pages via sidebar</li>
            </ul>
          </div>

          <div>
            <h3 className="font-semibold mb-2">🔧 Architecture:</h3>
            <pre className="text-xs bg-muted p-3 rounded-md overflow-x-auto">
{`SidebarProvider
├── <ErrorBoundary> (Sidebar)
│   └── AppSidebar
│       └── [Sidebar Content]
│
└── SidebarInset
    ├── <ErrorBoundary> (Header)
    │   └── Header
    │       └── [Header Content]
    │
    └── <ErrorBoundary> (Page)
        └── Page Content
            └── [Your Page] ← Error isolated here`}
            </pre>
          </div>

          <div>
            <h3 className="font-semibold mb-2">📝 Error Logging:</h3>
            <p className="text-sm text-muted-foreground">
              Check the browser console to see error logs from each boundary:
            </p>
            <ul className="list-disc list-inside space-y-1 text-xs text-muted-foreground mt-2">
              <li>[App] Sidebar error caught by boundary</li>
              <li>[App] Header error caught by boundary</li>
              <li>[App] Page error caught by boundary</li>
            </ul>
          </div>
        </CardContent>
      </Card>

      <Card className="border-amber-500">
        <CardHeader>
          <CardTitle className="text-amber-600">Note</CardTitle>
        </CardHeader>
        <CardContent className="text-sm text-muted-foreground">
          <p>
            Testing errors in Sidebar and Header requires modifying those
            components directly to throw errors. The page crash test demonstrates
            the isolation principle - when this page crashes, the Sidebar and
            Header continue working normally.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}

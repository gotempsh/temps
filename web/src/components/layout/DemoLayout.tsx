import { ReactNode } from 'react'
import { Link } from 'react-router-dom'

interface DemoLayoutProps {
  children: ReactNode
}

export function DemoLayout({ children }: DemoLayoutProps) {
  return (
    <div className="min-h-screen flex flex-col bg-background">
      {/* Demo mode header with Temps branding */}
      <header className="sticky top-0 z-50 w-full border-b bg-background/95 backdrop-blur supports-[backdrop-filter]:bg-background/60">
        <div className="flex h-14 items-center px-4 sm:px-8">
          <Link to="/projects" className="flex items-center space-x-2">
            <span className="font-bold text-xl">Temps</span>
          </Link>
          <div className="flex-1" />
          <span className="text-xs text-muted-foreground bg-muted px-2 py-1 rounded">
            Demo Mode
          </span>
        </div>
      </header>

      {/* Main content area */}
      <main className="flex-1 w-full py-6 px-4 sm:px-8">
        {children}
      </main>
    </div>
  )
}

import { useAuth } from '@/contexts/AuthContext'
import { Button } from '@/components/ui/button'
import { Info, LogOut } from 'lucide-react'

interface DemoBannerProps {
  /** Show the "Exit Demo" button */
  showExitButton?: boolean
}

/**
 * DemoBanner - Banner displayed at the top of the page in demo mode
 *
 * Shows a message indicating the user is in demo mode with limited access.
 * Optionally shows an "Exit Demo" button to clear the session and redirect to main domain.
 *
 * In subdomain mode (demo.<domain>), Exit Demo redirects to the main domain (<domain>).
 */
export function DemoBanner({ showExitButton = false }: DemoBannerProps) {
  const { logout } = useAuth()

  const handleExitDemo = async () => {
    try {
      await logout()
    } catch {
      // If logout fails, continue with redirect anyway
    }

    // Redirect to main domain (remove "demo." prefix from hostname)
    // e.g., demo.localho.st -> localho.st
    const currentHost = window.location.hostname
    if (currentHost.startsWith('demo.')) {
      const mainDomain = currentHost.replace(/^demo\./, '')
      const protocol = window.location.protocol
      window.location.href = `${protocol}//${mainDomain}/`
    } else {
      // If not on demo subdomain, just reload to go to login
      window.location.href = '/'
    }
  }

  return (
    <div className="bg-primary/10 border-b border-primary/20 px-4 py-2 w-full">
      <div className="flex items-center justify-center gap-4 text-sm text-primary">
        <div className="flex items-center gap-2">
          <Info className="h-4 w-4" />
          <span>
            <strong>Demo Mode</strong> - Viewing sample analytics data with
            limited features
          </span>
        </div>
        {showExitButton && (
          <Button
            variant="outline"
            size="sm"
            onClick={handleExitDemo}
            className="h-7 text-xs"
          >
            <LogOut className="h-3 w-3 mr-1" />
            Exit Demo
          </Button>
        )}
      </div>
    </div>
  )
}

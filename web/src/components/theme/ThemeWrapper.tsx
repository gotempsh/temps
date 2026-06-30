import { useTheme } from 'next-themes'
import { useEffect, useState } from 'react'

interface ThemeWrapperProps {
  children: React.ReactNode
}

export function ThemeWrapper({ children }: ThemeWrapperProps) {
  const [mounted, setMounted] = useState(false)
  const { theme } = useTheme()

  // Prevent hydration mismatch
  useEffect(() => {
    setMounted(true)
  }, [])

  if (!mounted) {
    return <div className="bg-background">{children}</div>
  }

  // `min-h-dvh` (dynamic viewport) tracks the actually-visible area so the shell
  // matches the sidebar/dock (which use the dvh-based viewport) instead of
  // overflowing by the mobile browser-chrome height — that mismatch was the
  // source of the extra vertical scroll + empty band on mobile.
  return <div className={`min-h-dvh bg-background ${theme}`}>{children}</div>
}

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

  return <div className={`min-h-screen bg-background ${theme}`}>{children}</div>
}

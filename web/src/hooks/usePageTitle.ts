import { useEffect } from 'react'

export function usePageTitle(title: string) {
  useEffect(() => {
    const previousTitle = document.title
    document.title = title ? `${title} - Temps` : 'Temps'

    return () => {
      document.title = previousTitle
    }
  }, [title])
}

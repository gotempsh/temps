import { useQuery } from '@tanstack/react-query'
import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DomainsManagement } from '@/components/domains/DomainsManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'

export function Domains() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const {
    data: domains,
    isLoading,
    refetch,
  } = useQuery({
    ...listDomainsOptions({}),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Domains' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to add new domain
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Check if user is typing in an input field
      const target = e.target as HTMLElement
      const isTyping =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable

      if (
        !isTyping &&
        e.key.toLowerCase() === 'n' &&
        !e.metaKey &&
        !e.ctrlKey &&
        !e.altKey &&
        !e.shiftKey
      ) {
        e.preventDefault()
        navigate('/domains/add')
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [navigate])

  usePageTitle('Domains')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <DomainsManagement
          domains={domains?.domains || []}
          isLoading={isLoading}
          reloadDomains={refetch}
        />
      </div>
    </div>
  )
}

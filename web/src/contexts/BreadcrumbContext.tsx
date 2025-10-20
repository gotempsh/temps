import { createContext, useContext, ReactNode, useState } from 'react'

export type BreadcrumbItem = {
  label: string
  href?: string
}

type BreadcrumbContextType = {
  breadcrumbs: BreadcrumbItem[]
  setBreadcrumbs: (items: BreadcrumbItem[]) => void
}

const BreadcrumbContext = createContext<BreadcrumbContextType | undefined>(
  undefined
)

export function BreadcrumbProvider({ children }: { children: ReactNode }) {
  const [breadcrumbs, setBreadcrumbs] = useState<BreadcrumbItem[]>([])

  return (
    <BreadcrumbContext.Provider value={{ breadcrumbs, setBreadcrumbs }}>
      {children}
    </BreadcrumbContext.Provider>
  )
}

export function useBreadcrumbs() {
  const context = useContext(BreadcrumbContext)
  if (context === undefined) {
    throw new Error('useBreadcrumbs must be used within a BreadcrumbProvider')
  }
  return context
}

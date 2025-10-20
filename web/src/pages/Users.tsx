import { useQuery } from '@tanstack/react-query'
import { listUsersOptions } from '@/api/client/@tanstack/react-query.gen'
import { UsersManagement } from '@/components/users/UsersManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect, useState } from 'react'
import { UserEditDialog } from '@/components/users/UserEditDialog'

export function Users() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [selectedUser, setSelectedUser] = useState<{
    id: number
    name: string
    email: string
  } | null>(null)

  const {
    data: users,
    isLoading,
    refetch,
  } = useQuery({
    ...listUsersOptions({
      query: {
        include_deleted: false,
      },
    }),
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Users' }])
  }, [setBreadcrumbs])

  usePageTitle('Users')

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <UsersManagement
          users={users}
          isLoading={isLoading}
          reloadUsers={refetch}
          onEditUser={setSelectedUser}
        />
      </div>
      {selectedUser && (
        <UserEditDialog
          onEdit={() => refetch()}
          user={selectedUser}
          open={!!selectedUser}
          onOpenChange={(open) => !open && setSelectedUser(null)}
        />
      )}
    </div>
  )
}

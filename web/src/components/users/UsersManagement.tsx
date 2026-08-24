'use client'

import {
  assignRoleMutation,
  deleteUserMutation,
  removeRoleMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RouteUserWithRoles } from '@/api/client/types.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { CreateActionButton } from '@/components/ui/create-action-button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Label } from '@/components/ui/label'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Edit2,
  MoreHorizontal,
  Plus,
  Shield,
  Trash2,
  UserPlus,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import { RolePermissionDetails } from './RolePermissionDetails'

const availableRoles = [
  {
    value: 'admin',
    label: 'Administrator',
    description: 'Full control of projects, users, settings, and the system.',
  },
  {
    value: 'user',
    label: 'User',
    description: 'Can deploy and operate every existing and future project.',
  },
]

interface UsersManagementProps {
  users?: RouteUserWithRoles[]
  isLoading: boolean
  reloadUsers: () => void
  onEditUser: (user: { id: number; name: string; email: string }) => void
}

export function UsersManagement({
  users,
  isLoading,
  reloadUsers,
  onEditUser,
}: UsersManagementProps) {
  const [userToDelete, setUserToDelete] = useState<number | null>(null)
  const [userToManageRoles, setUserToManageRoles] =
    useState<RouteUserWithRoles | null>(null)
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const deleteUser = useMutation({
    ...deleteUserMutation(),
    meta: {
      errorTitle: 'Failed to delete user',
    },
    onSuccess: () => {
      toast.success('User deleted successfully')
      setUserToDelete(null)
      reloadUsers()
    },
  })

  const assignRole = useMutation({
    ...assignRoleMutation(),
    meta: {
      errorTitle: 'Failed to assign role',
    },
    onSuccess: (_, variables) => {
      toast.success('Role updated successfully')
      // Update local state to reflect the change
      if (userToManageRoles) {
        setUserToManageRoles((prev) => {
          if (!prev) return null
          return {
            ...prev,
            roles: [
              {
                id: Date.now(),
                created_at: Date.now(),
                updated_at: Date.now(),
                name: variables.body.role_type,
              },
            ],
          }
        })
      }
      queryClient.invalidateQueries({ queryKey: ['listUsers'] })
      reloadUsers()
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => assignRole.mutate(variables))) {
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(problem.detail || problem.message || 'Failed to assign role')
    },
  })

  const removeRole = useMutation({
    ...removeRoleMutation(),
    meta: {
      errorTitle: 'Failed to remove role',
    },
    onSuccess: (_, variables) => {
      // Update local state to reflect the change
      if (userToManageRoles) {
        setUserToManageRoles((prev) => {
          if (!prev) return null
          return {
            ...prev,
            roles: prev.roles.filter(
              (r) => r.name !== variables.path.role_type
            ),
          }
        })
      }
      queryClient.invalidateQueries({ queryKey: ['listUsers'] })
      reloadUsers()
    },
  })

  const handleDeleteUser = async (userId: number) => {
    await deleteUser.mutateAsync({
      path: {
        user_id: userId,
      },
    })
  }

  const handleRoleChange = async (
    userId: number,
    roleType: string,
    currentRoles: string[]
  ) => {
    // Remove all existing roles first (since only one role can be active)
    for (const existingRole of currentRoles) {
      if (existingRole !== roleType) {
        await removeRole.mutateAsync({
          path: {
            user_id: userId,
            role_type: existingRole,
          },
        })
      }
    }

    // Check if the user already has this role
    const hasRole = currentRoles.includes(roleType)

    if (!hasRole) {
      // Assign the new role
      await assignRole.mutateAsync({
        path: {
          user_id: userId,
        },
        body: {
          role_type: roleType,
          user_id: userId,
        },
      })
    }

    // Keep the dialog open - don't close it
    // The user list will be refreshed automatically via onSuccess callbacks
  }

  return (
    <div className="space-y-4">
      {verificationDialog}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Users</h2>
          <p className="text-sm text-muted-foreground">
            Manage user access and roles
          </p>
        </div>
        <CreateActionButton
          onClick={() => navigate('/settings/users/new')}
          label="Add User"
          icon={<UserPlus className="h-4 w-4" />}
        />
      </div>

      <AlertDialog
        open={userToDelete !== null}
        onOpenChange={(open) => !open && setUserToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
              This action cannot be undone. This will permanently delete the
              user and remove their access to the platform.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => userToDelete && handleDeleteUser(userToDelete)}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              disabled={deleteUser.isPending}
            >
              {deleteUser.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <Dialog
        open={!!userToManageRoles}
        onOpenChange={(open) => !open && setUserToManageRoles(null)}
      >
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Manage User Role</DialogTitle>
            <p className="text-sm text-muted-foreground">
              Select a role for{' '}
              {userToManageRoles?.user.name || userToManageRoles?.user.username}
              . Only one role can be active at a time.
            </p>
          </DialogHeader>
          <div className="py-4">
            {userToManageRoles && (
              <RadioGroup
                value={
                  Array.from(
                    new Map(
                      userToManageRoles.roles.map((r) => [r.name, r])
                    ).values()
                  )[0]?.name || 'user'
                }
                onValueChange={(value) => {
                  const currentRoles = Array.from(
                    new Map(
                      userToManageRoles.roles.map((r) => [r.name, r])
                    ).values()
                  ).map((r) => r.name)
                  handleRoleChange(
                    userToManageRoles.user.id,
                    value,
                    currentRoles
                  )
                }}
                disabled={assignRole.isPending || removeRole.isPending}
                className="space-y-3"
              >
                {availableRoles.map((role) => {
                  const uniqueRoles = Array.from(
                    new Map(
                      userToManageRoles.roles.map((r) => [r.name, r])
                    ).values()
                  )
                  const isActive = uniqueRoles.some(
                    (r) => r.name === role.value
                  )
                  return (
                    <div
                      key={role.value}
                      className="flex items-start space-x-3 p-3 rounded-lg border bg-card hover:bg-accent/50 transition-colors cursor-pointer"
                    >
                      <RadioGroupItem
                        value={role.value}
                        id={role.value}
                        className="mt-1"
                      />
                      <Label
                        htmlFor={role.value}
                        className="flex-1 cursor-pointer space-y-1"
                      >
                        <div className="flex items-center gap-2">
                          <div className="text-sm font-medium">
                            {role.label}
                          </div>
                          {isActive && (
                            <Badge variant="secondary" className="text-xs">
                              Active
                            </Badge>
                          )}
                        </div>
                        <div className="text-sm text-muted-foreground">
                          {role.description}
                        </div>
                      </Label>
                    </div>
                  )
                })}
              </RadioGroup>
            )}
            {userToManageRoles && (
              <div className="mt-6 border-t border-border/60 pt-6">
                <RolePermissionDetails
                  roleNames={Array.from(
                    new Set(userToManageRoles.roles.map((role) => role.name))
                  )}
                />
              </div>
            )}
          </div>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setUserToManageRoles(null)}
              disabled={assignRole.isPending || removeRole.isPending}
            >
              {assignRole.isPending || removeRole.isPending
                ? 'Updating...'
                : 'Close'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {isLoading ? (
        <Card>
          <div className="p-4 space-y-4">
            {[...Array(3)].map((_, i) => (
              <div
                key={i}
                className="flex items-center justify-between py-4 animate-pulse"
              >
                <div className="flex items-center gap-4">
                  <div className="h-10 w-10 rounded-full bg-muted" />
                  <div className="space-y-2">
                    <div className="h-4 w-32 bg-muted rounded" />
                    <div className="h-4 w-48 bg-muted rounded" />
                  </div>
                </div>
                <div className="h-8 w-8 bg-muted rounded" />
              </div>
            ))}
          </div>
        </Card>
      ) : !users?.length ? (
        <EmptyState
          icon={UserPlus}
          title="No users found"
          description="Get started by creating a new user."
          action={
            <Button onClick={() => navigate('/settings/users/new')}>
              <Plus className="mr-2 h-4 w-4" />
              Add User
            </Button>
          }
        />
      ) : (
        <Card>
          <div className="divide-y">
            {users.map((user) => (
              <div
                key={user.user.id}
                role="button"
                tabIndex={0}
                onClick={() => navigate(`/settings/users/${user.user.id}`)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault()
                    navigate(`/settings/users/${user.user.id}`)
                  }
                }}
                className="flex cursor-pointer items-center justify-between p-4 transition-colors hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-4">
                    <Avatar className="h-10 w-10">
                      <AvatarImage src={user.user.image} />
                      <AvatarFallback>
                        {user.user.username?.slice(0, 2).toUpperCase() ||
                          user.user.name?.slice(0, 2).toUpperCase() ||
                          'U'}
                      </AvatarFallback>
                    </Avatar>
                    <div>
                      <div className="flex items-center gap-2">
                        <h3 className="truncate font-medium">
                          {user.user.name || user.user.username}
                        </h3>
                        <div className="flex flex-wrap gap-1">
                          {/* Deduplicate roles by name to prevent showing duplicates */}
                          {Array.from(
                            new Map(
                              user.roles.map((role) => [role.name, role])
                            ).values()
                          ).map((role) => (
                            <Badge
                              key={role.id}
                              variant="secondary"
                              className="text-xs"
                            >
                              {role.name}
                            </Badge>
                          ))}
                        </div>
                      </div>
                      <p className="text-sm text-muted-foreground">
                        {user.user.email}
                      </p>
                    </div>
                  </div>
                </div>
                <div className="ml-4" onClick={(e) => e.stopPropagation()}>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button variant="ghost" size="icon">
                        <MoreHorizontal className="h-4 w-4" />
                        <span className="sr-only">Open menu</span>
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onClick={() =>
                          onEditUser({
                            id: user.user.id,
                            name: user.user.username || user.user.name || '',
                            email: user.user.email || '',
                          })
                        }
                      >
                        <Edit2 className="mr-2 h-4 w-4" />
                        Edit
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() => setUserToManageRoles(user)}
                      >
                        <Shield className="mr-2 h-4 w-4" />
                        Manage Roles
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        onClick={() => setUserToDelete(user.user.id)}
                        className="text-destructive"
                      >
                        <Trash2 className="mr-2 h-4 w-4" />
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}
    </div>
  )
}

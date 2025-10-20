'use client'

import {
  assignRoleMutation,
  createUserMutation,
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
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ScrollArea } from '@/components/ui/scroll-area'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Edit2,
  Eye,
  EyeOff,
  MoreHorizontal,
  Plus,
  Shield,
  Trash2,
  UserPlus,
} from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'

const createUserSchema = z.object({
  name: z.string().min(3, 'Name must be at least 3 characters'),
  email: z.string().email('Invalid email address'),
  password: z.string().min(8, 'Password must be at least 8 characters'),
  role: z.string().min(1, 'Please select a role'),
})

type CreateUserFormData = z.infer<typeof createUserSchema>

const availableRoles = [
  {
    value: 'admin',
    label: 'Administrator',
    description: 'Full access to all features',
  },
  { value: 'user', label: 'User', description: 'Standard user access' },
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
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [userToDelete, setUserToDelete] = useState<number | null>(null)
  const [userToManageRoles, setUserToManageRoles] =
    useState<RouteUserWithRoles | null>(null)
  const [showPassword, setShowPassword] = useState(false)
  const queryClient = useQueryClient()

  const form = useForm<CreateUserFormData>({
    resolver: zodResolver(createUserSchema),
    defaultValues: {
      name: '',
      email: '',
      password: '',
      role: 'user',
    },
  })

  // Use register mutation for creating users with passwords
  const createUser = useMutation({
    ...createUserMutation(),
    meta: {
      errorTitle: 'Failed to create user',
    },
    onSuccess: async (data) => {
      // After creating the user, assign the selected role
      if (form.getValues('role') !== 'user') {
        // If admin role selected, we need to assign it
        // Note: This assumes the API allows role assignment after registration
        try {
          await assignRole.mutateAsync({
            path: {
              user_id: data.user.id,
            },
            body: {
              role_type: form.getValues('role'),
              user_id: data.user.id,
            },
          })
        } catch (error) {
          console.error('Failed to assign role:', error)
        }
      }

      toast.success('User created successfully')
      setIsCreateDialogOpen(false)
      form.reset()
      reloadUsers()
    },
  })

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
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listUsers'] })
      reloadUsers()
    },
  })

  const removeRole = useMutation({
    ...removeRoleMutation(),
    meta: {
      errorTitle: 'Failed to remove role',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['listUsers'] })
      reloadUsers()
    },
  })

  const handleCreateUser = async (data: CreateUserFormData) => {
    await createUser.mutateAsync({
      body: {
        username: data.name,
        email: data.email,
        roles: [data.role],
        // password: data.password,
      },
    })
  }

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
    hasRole: boolean
  ) => {
    if (hasRole) {
      await removeRole.mutateAsync({
        path: {
          user_id: userId,
          role_type: roleType,
        },
      })
    } else {
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

    if (userToManageRoles) {
      setUserToManageRoles(
        (prevUser: RouteUserWithRoles | null): RouteUserWithRoles | null => {
          if (!prevUser) return null
          return {
            ...prevUser,
            roles: hasRole
              ? prevUser.roles
              : [
                  ...prevUser.roles,
                  { id: 0, created_at: 0, updated_at: 0, name: roleType },
                ],
          }
        }
      )
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-lg font-semibold">Users</h2>
          <p className="text-sm text-muted-foreground">
            Manage user access and roles
          </p>
        </div>
        <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
          <DialogTrigger asChild>
            <Button>
              <UserPlus className="mr-2 h-4 w-4" />
              Add User
            </Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-[425px]">
            <DialogHeader>
              <DialogTitle>Create New User</DialogTitle>
            </DialogHeader>
            <Form {...form}>
              <form
                onSubmit={form.handleSubmit(handleCreateUser)}
                className="space-y-4"
              >
                <FormField
                  control={form.control}
                  name="name"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Name</FormLabel>
                      <FormControl>
                        <Input placeholder="John Doe" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="email"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Email</FormLabel>
                      <FormControl>
                        <Input
                          type="email"
                          placeholder="john@example.com"
                          {...field}
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="password"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Password</FormLabel>
                      <FormControl>
                        <div className="relative">
                          <Input
                            type={showPassword ? 'text' : 'password'}
                            placeholder="Enter password"
                            {...field}
                          />
                          <Button
                            type="button"
                            variant="ghost"
                            size="sm"
                            className="absolute right-0 top-0 h-full px-3 py-2 hover:bg-transparent"
                            onClick={() => setShowPassword(!showPassword)}
                          >
                            {showPassword ? (
                              <EyeOff className="h-4 w-4" />
                            ) : (
                              <Eye className="h-4 w-4" />
                            )}
                          </Button>
                        </div>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="role"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Role</FormLabel>
                      <Select
                        value={field.value}
                        onValueChange={field.onChange}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue placeholder="Select a role" />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {availableRoles.map((role) => (
                            <SelectItem key={role.value} value={role.value}>
                              <div>
                                <div className="font-medium">{role.label}</div>
                                <div className="text-xs text-muted-foreground">
                                  {role.description}
                                </div>
                              </div>
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <DialogFooter>
                  <Button type="submit" disabled={createUser.isPending}>
                    {createUser.isPending ? 'Creating...' : 'Create User'}
                  </Button>
                </DialogFooter>
              </form>
            </Form>
          </DialogContent>
        </Dialog>
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
        <DialogContent className="sm:max-w-[425px]">
          <DialogHeader>
            <DialogTitle>Manage User Roles</DialogTitle>
            <p className="text-sm text-muted-foreground">
              Update roles for{' '}
              {userToManageRoles?.user.name || userToManageRoles?.user.username}
            </p>
          </DialogHeader>
          <div className="py-4">
            <ScrollArea className="h-[300px] pr-4">
              <div className="space-y-4">
                {availableRoles.map((role) => {
                  const hasRole = userToManageRoles?.roles.some(
                    (r) => r.name === role.value
                  )
                  return (
                    <div
                      key={role.value}
                      className="flex items-center justify-between"
                    >
                      <div className="space-y-0.5">
                        <div className="text-sm font-medium">{role.label}</div>
                        <div className="text-sm text-muted-foreground">
                          {role.description}
                        </div>
                      </div>
                      <Switch
                        checked={hasRole}
                        disabled={assignRole.isPending || removeRole.isPending}
                        onCheckedChange={(checked) =>
                          userToManageRoles &&
                          handleRoleChange(
                            userToManageRoles.user.id,
                            role.value,
                            !checked
                          )
                        }
                      />
                    </div>
                  )
                })}
              </div>
            </ScrollArea>
          </div>
          <DialogFooter>
            <Button
              variant="secondary"
              onClick={() => setUserToManageRoles(null)}
            >
              Done
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
          description="Get started by creating a new user"
          action={
            <Button onClick={() => setIsCreateDialogOpen(true)}>
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
                className="flex items-center justify-between p-4"
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
                          {user.roles.map((role) => (
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
                <div className="ml-4">
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

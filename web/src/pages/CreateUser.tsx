import {
  addTeamMemberMutation,
  createUserMutation,
  getApiKeyPermissionsOptions,
  listTeamsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { TeamRole } from '@/api/client'
import { RolePermissionDetails } from '@/components/users/RolePermissionDetails'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  PASSWORD_REQUIREMENTS,
  generateCompliantPassword,
  passwordRequirementResults,
  passwordSchema,
} from '@/lib/password-policy'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Check,
  Circle,
  Eye,
  EyeOff,
  Loader2,
  WandSparkles,
} from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link, useNavigate } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

const createUserSchema = z.object({
  name: z.string().min(3, 'Name must be at least 3 characters'),
  email: z.email('Enter a valid email address'),
  password: passwordSchema,
  role: z.enum(['user', 'admin']),
  mustChangePassword: z.boolean(),
  teamId: z.string(),
  teamRole: z.enum(['owner', 'admin', 'deployer', 'viewer']),
})

type CreateUserFormData = z.infer<typeof createUserSchema>

const ROLE_OPTIONS = [
  {
    value: 'user' as const,
    label: 'User',
    description:
      'Can deploy and operate every existing and future project, but cannot manage users or system settings.',
  },
  {
    value: 'admin' as const,
    label: 'Administrator',
    description:
      'Full control of projects, users, roles, settings, and system resources.',
  },
]

const TEAM_ROLE_OPTIONS: Array<{
  value: TeamRole
  label: string
  description: string
}> = [
  { value: 'viewer', label: 'Viewer', description: 'Can view team projects.' },
  {
    value: 'deployer',
    label: 'Deployer',
    description: 'Can view and deploy team projects.',
  },
  {
    value: 'admin',
    label: 'Team admin',
    description: 'Can operate projects and manage team members.',
  },
  {
    value: 'owner',
    label: 'Team owner',
    description: 'Full control of the team and its project access.',
  },
]

export function CreateUser() {
  usePageTitle('Create user')
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [showPassword, setShowPassword] = useState(false)
  const [teamAssignmentFailure, setTeamAssignmentFailure] = useState<{
    userId: number
    teamId: number
    role: TeamRole
  } | null>(null)
  const pendingTeamAssignment = useRef<{
    teamId: number
    role: TeamRole
  } | null>(null)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Users', href: '/settings/users' },
      { label: 'Create user' },
    ])
  }, [setBreadcrumbs])

  const permissionsQuery = useQuery({
    ...getApiKeyPermissionsOptions({}),
  })
  const teamsQuery = useQuery(
    listTeamsOptions({ query: { page: 1, page_size: 100 } })
  )

  const form = useForm<CreateUserFormData>({
    resolver: zodResolver(createUserSchema),
    defaultValues: {
      name: '',
      email: '',
      password: '',
      role: 'user',
      mustChangePassword: true,
      teamId: '',
      teamRole: 'viewer',
    },
  })

  const password = form.watch('password')
  const selectedRole = form.watch('role')
  const selectedTeamId = form.watch('teamId')
  const selectedTeamRole = form.watch('teamRole')
  const requirementResults = passwordRequirementResults(password)

  const addTeamMember = useMutation({
    ...addTeamMemberMutation(),
  })

  const createUser = useMutation({
    ...createUserMutation(),
    meta: { errorTitle: 'Failed to create user' },
    onSuccess: async (createdUser) => {
      const teamAssignment = pendingTeamAssignment.current
      if (teamAssignment) {
        try {
          await addTeamMember.mutateAsync({
            path: { team_id: teamAssignment.teamId },
            body: {
              user_id: createdUser.user.id,
              role: teamAssignment.role,
            },
          })
          toast.success('User created and added to the team')
        } catch {
          setTeamAssignmentFailure({
            userId: createdUser.user.id,
            teamId: teamAssignment.teamId,
            role: teamAssignment.role,
          })
          toast.error(
            'User created, but team assignment failed. Retry it before leaving this page.'
          )
          await queryClient.invalidateQueries({ queryKey: ['listUsers'] })
          return
        }
      } else {
        toast.success('User created successfully')
      }
      await queryClient.invalidateQueries({ queryKey: ['listUsers'] })
      navigate('/settings/users')
    },
  })

  const handleSubmit = async (data: CreateUserFormData) => {
    if (teamAssignmentFailure) return
    pendingTeamAssignment.current = data.teamId
      ? { teamId: Number(data.teamId), role: data.teamRole }
      : null
    await createUser.mutateAsync({
      body: {
        username: data.name,
        email: data.email,
        password: data.password,
        roles: [data.role],
        must_change_password: data.mustChangePassword,
      },
    })
  }

  const retryTeamAssignment = async () => {
    if (!teamAssignmentFailure) return
    try {
      await addTeamMember.mutateAsync({
        path: { team_id: teamAssignmentFailure.teamId },
        body: {
          user_id: teamAssignmentFailure.userId,
          role: teamAssignmentFailure.role,
        },
      })
      setTeamAssignmentFailure(null)
      toast.success('User added to the team')
      navigate('/settings/users')
    } catch {
      toast.error(
        'Team assignment still failed. The account was not created again; you can retry safely.'
      )
    }
  }

  const selectedRoleAvailable = permissionsQuery.data?.roles.some(
    (role) => role.name === selectedRole
  )

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="space-y-1">
          <Button type="button" variant="ghost" size="sm" asChild>
            <Link to="/settings/users">
              <ArrowLeft className="size-4 shrink-0" />
              Back to users
            </Link>
          </Button>
          <div>
            <h1 className="text-balance text-2xl font-semibold tracking-tight">
              Create user
            </h1>
            <p className="max-w-[65ch] text-pretty text-base text-muted-foreground sm:text-sm">
              Create login credentials, choose the platform role, and review
              every permission before granting access.
            </p>
          </div>
        </div>
      </div>

      <Form {...form}>
        <form
          onSubmit={form.handleSubmit(handleSubmit)}
          className="grid gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(20rem,2fr)] lg:items-start"
        >
          <div className="min-w-0 space-y-6">
            <Card>
              <CardHeader>
                <CardTitle>Account details</CardTitle>
              </CardHeader>
              <CardContent className="space-y-5">
                <div className="grid gap-5 sm:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Name</FormLabel>
                        <FormControl>
                          <Input
                            placeholder="Alex Morgan"
                            autoComplete="name"
                            {...field}
                          />
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
                            placeholder="alex@example.com"
                            autoComplete="email"
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>

                <FormField
                  control={form.control}
                  name="password"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Temporary password</FormLabel>
                      <FormControl>
                        <div className="relative">
                          <Input
                            type={showPassword ? 'text' : 'password'}
                            placeholder="Enter a temporary password"
                            autoComplete="new-password"
                            className="pr-20"
                            {...field}
                          />
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                type="button"
                                variant="ghost"
                                size="icon"
                                className="absolute inset-y-0 right-10 text-muted-foreground hover:text-foreground"
                                onClick={() => {
                                  field.onChange(generateCompliantPassword())
                                  setShowPassword(true)
                                }}
                                aria-label="Generate a secure temporary password"
                              >
                                <WandSparkles className="size-4 shrink-0" />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>
                              Generate secure password
                            </TooltipContent>
                          </Tooltip>
                          <Button
                            type="button"
                            variant="ghost"
                            size="icon"
                            className="absolute inset-y-0 right-0"
                            onClick={() =>
                              setShowPassword((visible) => !visible)
                            }
                          >
                            {showPassword ? (
                              <EyeOff className="size-4 shrink-0" />
                            ) : (
                              <Eye className="size-4 shrink-0" />
                            )}
                            <span className="sr-only">
                              {showPassword ? 'Hide password' : 'Show password'}
                            </span>
                          </Button>
                        </div>
                      </FormControl>
                      <FormDescription>
                        The password must meet every requirement below.
                      </FormDescription>
                      <FormMessage />
                      <ul role="list" className="grid gap-2 sm:grid-cols-2">
                        {PASSWORD_REQUIREMENTS.map((requirement, index) => {
                          const met = requirementResults[index]?.met ?? false
                          return (
                            <li
                              key={requirement.id}
                              className="flex items-baseline gap-2 text-base text-muted-foreground sm:text-sm"
                            >
                              {met ? (
                                <Check className="size-4 h-lh shrink-0 stroke-emerald-600" />
                              ) : (
                                <Circle className="size-4 h-lh shrink-0 stroke-muted-foreground" />
                              )}
                              <div>{requirement.label}</div>
                            </li>
                          )
                        })}
                      </ul>
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="mustChangePassword"
                  render={({ field }) => (
                    <FormItem className="rounded-lg bg-muted/50 p-4">
                      <div className="flex items-start gap-3">
                        <FormControl>
                          <Checkbox
                            checked={field.value}
                            onCheckedChange={field.onChange}
                          />
                        </FormControl>
                        <div className="min-w-0 space-y-1">
                          <FormLabel>
                            Require a new password at first sign-in
                          </FormLabel>
                          <FormDescription className="text-pretty text-base sm:text-sm">
                            The temporary password only starts a protected
                            password-change session. The user cannot access the
                            platform until they choose a new password.
                          </FormDescription>
                        </div>
                      </div>
                    </FormItem>
                  )}
                />
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Role</CardTitle>
              </CardHeader>
              <CardContent>
                <FormField
                  control={form.control}
                  name="role"
                  render={({ field }) => (
                    <FormItem>
                      <FormControl>
                        <RadioGroup
                          value={field.value}
                          onValueChange={field.onChange}
                          className="grid gap-3 sm:grid-cols-2"
                        >
                          {ROLE_OPTIONS.map((role) => (
                            <Label
                              key={role.value}
                              htmlFor={`role-${role.value}`}
                              className="flex cursor-pointer items-start gap-3 rounded-lg border border-border/60 p-4 has-[[data-state=checked]]:border-primary has-[[data-state=checked]]:bg-primary/5"
                            >
                              <RadioGroupItem
                                id={`role-${role.value}`}
                                value={role.value}
                                className="shrink-0"
                              />
                              <div className="min-w-0 space-y-1">
                                <div className="font-medium">{role.label}</div>
                                <p className="text-pretty text-base font-normal text-muted-foreground sm:text-sm">
                                  {role.description}
                                </p>
                              </div>
                            </Label>
                          ))}
                        </RadioGroup>
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Team membership</CardTitle>
              </CardHeader>
              <CardContent className="space-y-5">
                <div className="grid gap-5 sm:grid-cols-2">
                  <FormField
                    control={form.control}
                    name="teamId"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Team</FormLabel>
                        <Select
                          value={field.value || '__none__'}
                          onValueChange={(value) =>
                            field.onChange(value === '__none__' ? '' : value)
                          }
                          disabled={teamsQuery.isLoading || teamsQuery.isError}
                        >
                          <FormControl>
                            <SelectTrigger>
                              <SelectValue
                                placeholder={
                                  teamsQuery.isLoading
                                    ? 'Loading teams…'
                                    : 'No team'
                                }
                              />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            <SelectItem value="__none__">No team</SelectItem>
                            {teamsQuery.data?.teams.map((team) => (
                              <SelectItem key={team.id} value={String(team.id)}>
                                {team.name}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <FormDescription>
                          Optional. Adds the user to the selected team after
                          creating their account.
                        </FormDescription>
                        {teamsQuery.isError && (
                          <p className="text-sm text-destructive">
                            Teams could not be loaded. You can still create the
                            user and add them from the Teams page later.
                          </p>
                        )}
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <FormField
                    control={form.control}
                    name="teamRole"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Team role</FormLabel>
                        <Select
                          value={field.value}
                          onValueChange={field.onChange}
                          disabled={!selectedTeamId}
                        >
                          <FormControl>
                            <SelectTrigger>
                              <SelectValue />
                            </SelectTrigger>
                          </FormControl>
                          <SelectContent>
                            {TEAM_ROLE_OPTIONS.map((role) => (
                              <SelectItem key={role.value} value={role.value}>
                                {role.label}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                        <FormDescription>
                          {
                            TEAM_ROLE_OPTIONS.find(
                              (role) => role.value === selectedTeamRole
                            )?.description
                          }
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>

                {selectedTeamId && (
                  <div className="rounded-lg border border-border/60 bg-muted/40 p-4 text-sm text-muted-foreground">
                    Team membership and the platform role are separate. A User
                    still has all-project access from their platform role;
                    joining a team does not narrow those permissions.
                  </div>
                )}
              </CardContent>
            </Card>

            <div className="flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
              {teamAssignmentFailure ? (
                <Alert className="w-full text-left">
                  <AlertTitle>
                    Account created; team assignment pending
                  </AlertTitle>
                  <AlertDescription className="space-y-3">
                    <p>
                      The account is ready, but it has not been added to the
                      selected team. Retry safely without creating a duplicate
                      account.
                    </p>
                    <div className="flex flex-wrap gap-3">
                      <Button
                        type="button"
                        onClick={retryTeamAssignment}
                        disabled={addTeamMember.isPending}
                      >
                        {addTeamMember.isPending && (
                          <Loader2 className="size-4 shrink-0 animate-spin" />
                        )}
                        Retry team assignment
                      </Button>
                      <Button type="button" variant="outline" asChild>
                        <Link to="/settings/users">Finish without team</Link>
                      </Button>
                    </div>
                  </AlertDescription>
                </Alert>
              ) : (
                <>
                  <Button type="button" variant="outline" asChild>
                    <Link to="/settings/users">Cancel</Link>
                  </Button>
                  <Button
                    type="submit"
                    disabled={
                      createUser.isPending ||
                      addTeamMember.isPending ||
                      permissionsQuery.isLoading ||
                      permissionsQuery.isError ||
                      !selectedRoleAvailable
                    }
                  >
                    {createUser.isPending && (
                      <Loader2 className="size-4 shrink-0 animate-spin" />
                    )}
                    {createUser.isPending ? 'Creating user…' : 'Create user'}
                  </Button>
                </>
              )}
            </div>
          </div>

          <Card className="min-w-0 lg:sticky lg:top-4">
            <CardHeader>
              <CardTitle>
                {selectedRole === 'admin' ? 'Administrator' : 'User'} access
              </CardTitle>
            </CardHeader>
            <CardContent>
              <RolePermissionDetails roleNames={[selectedRole]} />
            </CardContent>
          </Card>
        </form>
      </Form>
    </div>
  )
}

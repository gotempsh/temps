import {
  changePasswordSelfMutation,
  disableMfaMutation,
  getCurrentUserOptions,
  setupMfaMutation,
  updateSelfMutation,
  verifyAndEnableMfaMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from '@/components/ui/dialog'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { MfaSetupResponse } from '@/api/client'
import { useAuth } from '@/contexts/AuthContext'

const formSchema = z.object({
  name: z.string().min(2, 'Name must be at least 2 characters'),
  email: z.string().email('Invalid email address'),
})

type FormValues = z.infer<typeof formSchema>

// Change-password form. Current + new + confirm + optional MFA + revoke.
// Server enforces complexity (>=8 chars, etc.) so we only do the obvious
// client-side checks: non-empty current, min-8 new, confirm matches, MFA
// is exactly 6 digits when provided.
const passwordSchema = z
  .object({
    current_password: z.string().min(1, 'Current password is required'),
    new_password: z
      .string()
      .min(8, 'Password must be at least 8 characters'),
    confirm_password: z.string().min(1, 'Please confirm your new password'),
    mfa_code: z
      .string()
      .optional()
      .refine(
        (v) => !v || /^\d{6}$/.test(v) || v.length >= 8,
        'Enter a 6-digit TOTP code or a recovery code',
      ),
    revoke_other_sessions: z.boolean(),
  })
  .refine((data) => data.new_password === data.confirm_password, {
    message: 'Passwords do not match',
    path: ['confirm_password'],
  })
  .refine((data) => data.new_password !== data.current_password, {
    message: 'New password must differ from the current one',
    path: ['new_password'],
  })

type PasswordValues = z.infer<typeof passwordSchema>

const mfaVerifySchema = z.object({
  code: z.string().length(6, 'Code must be 6 digits'),
})

type MfaVerifyValues = z.infer<typeof mfaVerifySchema>

const mfaDisableSchema = z.object({
  code: z.string().length(6, 'Code must be 6 digits'),
})

type MfaDisableValues = z.infer<typeof mfaDisableSchema>

export function Account() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()

  const { data: user, isLoading } = useQuery({
    ...getCurrentUserOptions(),
  })
  const { refetch } = useAuth()
  const [showMfaDialog, setShowMfaDialog] = useState(false)
  const [mfaSetupData, setMfaSetupData] = useState<MfaSetupResponse | null>(
    null
  )
  const [showDisableMfaDialog, setShowDisableMfaDialog] = useState(false)

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: user
      ? {
          name: user.name,
          email: user.email ?? '',
        }
      : {
          name: '',
          email: '',
        },
  })

  const { mutate: updateUser, isPending } = useMutation({
    ...updateSelfMutation(),
    meta: {
      errorTitle: 'Failed to update account',
    },
    onSuccess: () => {
      toast.success('Account updated successfully')
      refetch()
    },
  })

  // Change-password form. Server requires current_password as the re-auth
  // gate; mfa_code is required iff the account has MFA enabled (we mirror
  // that with a conditional field below). revoke_other_sessions defaults to
  // false to match the backend default — checking it kicks every other
  // session on submit.
  const passwordForm = useForm<PasswordValues>({
    resolver: zodResolver(passwordSchema),
    defaultValues: {
      current_password: '',
      new_password: '',
      confirm_password: '',
      mfa_code: '',
      revoke_other_sessions: false,
    },
  })

  const { mutate: changePassword, isPending: isChangingPassword } = useMutation({
    ...changePasswordSelfMutation(),
    meta: {
      errorTitle: 'Failed to change password',
    },
    onSuccess: () => {
      toast.success('Password changed successfully')
      passwordForm.reset()
    },
  })

  const mfaForm = useForm<MfaVerifyValues>({
    resolver: zodResolver(mfaVerifySchema),
    defaultValues: {
      code: '',
    },
  })

  const { mutate: setupMfa, isPending: isSettingUpMfa } = useMutation({
    ...setupMfaMutation(),
    meta: {
      errorTitle: 'Failed to setup MFA',
    },
    onSuccess: (data) => {
      setMfaSetupData(data)
      setShowMfaDialog(true)
    },
  })

  const { mutate: verifyMfa, isPending: isVerifyingMfa } = useMutation({
    ...verifyAndEnableMfaMutation(),
    meta: {
      errorTitle: 'Failed to enable MFA',
    },
    onSuccess: () => {
      toast.success('MFA enabled successfully')
      setShowMfaDialog(false)
      refetch()
    },
  })

  const mfaDisableForm = useForm<MfaDisableValues>({
    resolver: zodResolver(mfaDisableSchema),
    defaultValues: {
      code: '',
    },
  })

  const { mutate: disableMfa, isPending: isDisablingMfa } = useMutation({
    ...disableMfaMutation(),
    meta: {
      errorTitle: 'Failed to disable MFA',
    },
    onSuccess: () => {
      toast.success('MFA disabled successfully')
      setShowDisableMfaDialog(false)
      refetch()
      queryClient.invalidateQueries({
        queryKey: getCurrentUserOptions().queryKey,
      })
      mfaDisableForm.reset()
    },
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Account' }])
  }, [setBreadcrumbs])

  usePageTitle('Account')

  function onSubmit(data: FormValues) {
    updateUser({
      body: data,
    })
  }

  const onChangePassword = (data: PasswordValues) => {
    changePassword({
      body: {
        current_password: data.current_password,
        new_password: data.new_password,
        // Empty string = "no MFA code provided." Server treats this as
        // missing for MFA-enabled users and rejects with MfaCodeRequired,
        // which is correct.
        mfa_code: data.mfa_code?.length ? data.mfa_code : null,
        revoke_other_sessions: data.revoke_other_sessions,
      },
    })
  }

  const onVerifyMfa = (data: MfaVerifyValues) => {
    verifyMfa({
      body: { code: data.code },
    })
  }

  const onDisableMfa = (data: MfaDisableValues) => {
    disableMfa({
      body: { code: data.code },
    })
  }

  if (isLoading) {
    return <AccountSkeleton />
  }

  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Account Settings</CardTitle>
          <CardDescription>Manage your account information</CardDescription>
        </CardHeader>
        <CardContent>
          <Form {...form}>
            <form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
              <FormField
                control={form.control}
                name="name"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Name</FormLabel>
                    <FormControl>
                      <Input {...field} />
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
                      <Input {...field} type="email" />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {/* Role — read-only. The user can't change their own role
                  here; that requires an admin via /users. Surfacing it
                  reduces the "what permissions do I have?" question that
                  drives a lot of console support traffic. */}
              {user?.role && (
                <div className="space-y-2">
                  <FormLabel>Role</FormLabel>
                  <div className="flex items-center gap-2">
                    <Badge variant={user.role === 'admin' ? 'default' : 'secondary'}>
                      {user.role}
                    </Badge>
                    <span className="text-xs text-muted-foreground">
                      Contact an administrator to change your role.
                    </span>
                  </div>
                </div>
              )}
              <div className="flex justify-end">
                <Button type="submit" disabled={isPending}>
                  {isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Save Changes
                </Button>
              </div>
            </form>
          </Form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Change Password</CardTitle>
          <CardDescription>
            Update your account password. You'll need your current password to
            confirm the change.
            {user?.mfa_enabled
              ? ' Because you have MFA enabled, a TOTP code (or recovery code) is also required.'
              : ''}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Form {...passwordForm}>
            <form
              onSubmit={passwordForm.handleSubmit(onChangePassword)}
              className="space-y-4"
            >
              <FormField
                control={passwordForm.control}
                name="current_password"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Current password</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        autoComplete="current-password"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={passwordForm.control}
                name="new_password"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>New password</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        autoComplete="new-password"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={passwordForm.control}
                name="confirm_password"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Confirm new password</FormLabel>
                    <FormControl>
                      <Input
                        type="password"
                        autoComplete="new-password"
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              {user?.mfa_enabled && (
                <FormField
                  control={passwordForm.control}
                  name="mfa_code"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>MFA code</FormLabel>
                      <FormControl>
                        <Input
                          inputMode="numeric"
                          autoComplete="one-time-code"
                          placeholder="6-digit TOTP or recovery code"
                          {...field}
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              )}
              <FormField
                control={passwordForm.control}
                name="revoke_other_sessions"
                render={({ field }) => (
                  <FormItem className="flex flex-row items-start space-x-2 space-y-0">
                    <FormControl>
                      <Checkbox
                        checked={field.value}
                        onCheckedChange={field.onChange}
                      />
                    </FormControl>
                    <div className="space-y-0.5 leading-none">
                      <FormLabel className="font-normal cursor-pointer">
                        Sign out of all other sessions
                      </FormLabel>
                      <p className="text-xs text-muted-foreground">
                        Revokes every session except this one. Recommended if
                        you're rotating because of a leak or shared device.
                      </p>
                    </div>
                  </FormItem>
                )}
              />
              <div className="flex justify-end">
                <Button type="submit" disabled={isChangingPassword}>
                  {isChangingPassword && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Change password
                </Button>
              </div>
            </form>
          </Form>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Two-Factor Authentication</CardTitle>
          <CardDescription>
            Add an extra layer of security to your account by enabling
            two-factor authentication
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {user?.mfa_enabled ? (
            <div className="space-y-4">
              <Alert>
                <AlertDescription>
                  Two-factor authentication is currently enabled for your
                  account.
                </AlertDescription>
              </Alert>
              <Button
                variant="destructive"
                onClick={() => setShowDisableMfaDialog(true)}
                disabled={isDisablingMfa}
              >
                {isDisablingMfa && (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                )}
                Disable 2FA
              </Button>
            </div>
          ) : (
            <Button onClick={() => setupMfa({})} disabled={isSettingUpMfa}>
              {isSettingUpMfa && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Setup 2FA
            </Button>
          )}
        </CardContent>
      </Card>

      <Dialog open={showMfaDialog} onOpenChange={setShowMfaDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Setup Two-Factor Authentication</DialogTitle>
            <DialogDescription>
              Scan the QR code with your authenticator app and enter the
              verification code below.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            {mfaSetupData?.qr_code && (
              <div className="flex justify-center">
                <img
                  src={mfaSetupData.qr_code}
                  alt="QR Code for 2FA"
                  className="w-48 h-48"
                />
              </div>
            )}
            <div className="text-sm text-muted-foreground text-center">
              If you can&apos;t scan the QR code, enter this code manually:
              <br />
              <code className="font-mono bg-muted px-2 py-1 rounded">
                {mfaSetupData?.secret_key}
              </code>
            </div>
            <Form {...mfaForm}>
              <form
                onSubmit={mfaForm.handleSubmit(onVerifyMfa)}
                className="space-y-4"
              >
                <FormField
                  control={mfaForm.control}
                  name="code"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Verification Code</FormLabel>
                      <FormControl>
                        <Input {...field} placeholder="Enter 6-digit code" />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <div className="flex justify-end">
                  <Button type="submit" disabled={isVerifyingMfa}>
                    {isVerifyingMfa && (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    )}
                    Verify and Enable
                  </Button>
                </div>
              </form>
            </Form>
          </div>
        </DialogContent>
      </Dialog>

      <Dialog
        open={showDisableMfaDialog}
        onOpenChange={setShowDisableMfaDialog}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Disable Two-Factor Authentication</DialogTitle>
            <DialogDescription>
              Please enter your 2FA code to confirm disabling two-factor
              authentication. This will make your account less secure.
            </DialogDescription>
          </DialogHeader>
          <Form {...mfaDisableForm}>
            <form
              onSubmit={mfaDisableForm.handleSubmit(onDisableMfa)}
              className="space-y-4"
            >
              <FormField
                control={mfaDisableForm.control}
                name="code"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Verification Code</FormLabel>
                    <FormControl>
                      <Input {...field} placeholder="Enter 6-digit code" />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <div className="flex justify-end gap-2">
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    setShowDisableMfaDialog(false)
                    mfaDisableForm.reset()
                  }}
                >
                  Cancel
                </Button>
                <Button
                  type="submit"
                  variant="destructive"
                  disabled={isDisablingMfa}
                >
                  {isDisablingMfa && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Disable 2FA
                </Button>
              </div>
            </form>
          </Form>
        </DialogContent>
      </Dialog>
    </div>
  )
}

function AccountSkeleton() {
  return (
    <div className="max-w-2xl mx-auto space-y-6">
      <Card>
        <CardHeader>
          <Skeleton className="h-8 w-[200px]" />
          <Skeleton className="h-4 w-[300px]" />
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-2">
            <Skeleton className="h-4 w-[100px]" />
            <Skeleton className="h-10 w-full" />
          </div>
          <div className="space-y-2">
            <Skeleton className="h-4 w-[100px]" />
            <Skeleton className="h-10 w-full" />
          </div>
          <div className="flex justify-end">
            <Skeleton className="h-10 w-[120px]" />
          </div>
        </CardContent>
      </Card>
    </div>
  )
}

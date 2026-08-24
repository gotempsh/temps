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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { usePageTitle } from '@/hooks/usePageTitle'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, Loader2, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import { z } from 'zod'
import { MfaSetupResponse } from '@/api/client'
import { useAuth } from '@/contexts/AuthContext'
import { cn } from '@/lib/utils'
import {
  PASSWORD_REQUIREMENTS,
  passwordRequirementResults,
  passwordSchema as passwordPolicySchema,
} from '@/lib/password-policy'

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
    new_password: passwordPolicySchema,
    confirm_password: z.string().min(1, 'Please confirm your new password'),
    mfa_code: z
      .string()
      .optional()
      .refine(
        (v) => !v || /^\d{6}$/.test(v) || v.length >= 8,
        'Enter a 6-digit TOTP code or a recovery code'
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

// Current-password confirmation gate for MFA enrollment.
// Accounts with no password set (SSO-only) may leave this blank;
// the server skips the check for those accounts.
const mfaSetupPasswordSchema = z.object({
  current_password: z.string().optional(),
})

type MfaSetupPasswordValues = z.infer<typeof mfaSetupPasswordSchema>

export function Account() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()

  const { data: user, isLoading } = useQuery({
    ...getCurrentUserOptions(),
  })
  const { refetch } = useAuth()
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const [showMfaDialog, setShowMfaDialog] = useState(false)
  const [mfaSetupData, setMfaSetupData] = useState<MfaSetupResponse | null>(
    null
  )
  const [showDisableMfaDialog, setShowDisableMfaDialog] = useState(false)
  const [showMfaPasswordDialog, setShowMfaPasswordDialog] = useState(false)

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
    onError: (error, variables) => {
      // Only email changes are step-up gated server-side, but it's safe to
      // intercept unconditionally — this only fires on an actual 428.
      if (handleSensitiveActionError(error, () => updateUser(variables))) {
        return
      }
      const problem = error as { detail?: string; message?: string }
      toast.error(
        problem.detail || problem.message || 'Failed to update account'
      )
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
  const newPassword = passwordForm.watch('new_password')
  const requirementResults = passwordRequirementResults(newPassword)

  const { mutate: changePassword, isPending: isChangingPassword } = useMutation(
    {
      ...changePasswordSelfMutation(),
      meta: {
        errorTitle: 'Failed to change password',
      },
      onSuccess: () => {
        toast.success('Password changed successfully')
        passwordForm.reset()
      },
    }
  )

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
      setShowMfaPasswordDialog(false)
      mfaSetupPasswordForm.reset()
      setShowMfaDialog(true)
    },
  })

  const onStartMfaSetup = (data: MfaSetupPasswordValues) => {
    setupMfa({
      body: {
        current_password: data.current_password || null,
      },
    })
  }

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

  const mfaSetupPasswordForm = useForm<MfaSetupPasswordValues>({
    resolver: zodResolver(mfaSetupPasswordSchema),
    defaultValues: {
      current_password: '',
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
      {verificationDialog}
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
                    <Badge
                      variant={user.role === 'admin' ? 'default' : 'secondary'}
                    >
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
                        disabled={isChangingPassword}
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
                        disabled={isChangingPassword}
                        {...field}
                      />
                    </FormControl>
                    <FormMessage />
                    <ul
                      role="list"
                      className="grid gap-1.5 pt-2 sm:grid-cols-2"
                    >
                      {PASSWORD_REQUIREMENTS.map((requirement, index) => {
                        const met =
                          requirementResults[index]?.met ?? false
                        return (
                          <li
                            key={requirement.id}
                            className={cn(
                              'flex items-center gap-1.5 text-xs transition-colors',
                              met
                                ? 'text-emerald-600 dark:text-emerald-400 font-medium'
                                : 'text-rose-500 font-medium'
                            )}
                          >
                            {met ? (
                              <Check className="h-3.5 w-3.5 shrink-0 stroke-[2.5]" />
                            ) : (
                              <X className="h-3.5 w-3.5 shrink-0 stroke-[2.5]" />
                            )}
                            <span>{requirement.label}</span>
                          </li>
                        )
                      })}
                    </ul>
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
                        disabled={isChangingPassword}
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
                          disabled={isChangingPassword}
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
                        disabled={isChangingPassword}
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
            <Button
              onClick={() => setShowMfaPasswordDialog(true)}
              disabled={isSettingUpMfa}
            >
              {isSettingUpMfa && (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              )}
              Setup 2FA
            </Button>
          )}
        </CardContent>
      </Card>

      {/* Current-password confirmation before generating a new MFA secret.
          SSO-only accounts (no local password) may leave the field empty;
          the server skips the check when no password hash is set. */}
      <Dialog
        open={showMfaPasswordDialog}
        onOpenChange={(open) => {
          setShowMfaPasswordDialog(open)
          if (!open) mfaSetupPasswordForm.reset()
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Confirm Identity to Setup 2FA</DialogTitle>
            <DialogDescription>
              Enter your current password to begin two-factor authentication
              setup. If your account uses SSO and has no local password, leave
              this field empty.
            </DialogDescription>
          </DialogHeader>
          <Form {...mfaSetupPasswordForm}>
            <form
              onSubmit={mfaSetupPasswordForm.handleSubmit(onStartMfaSetup)}
              className="space-y-4"
            >
              <FormField
                control={mfaSetupPasswordForm.control}
                name="current_password"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Current Password</FormLabel>
                    <FormControl>
                      <Input
                        {...field}
                        type="password"
                        placeholder="Enter current password"
                        autoComplete="current-password"
                      />
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
                    setShowMfaPasswordDialog(false)
                    mfaSetupPasswordForm.reset()
                  }}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={isSettingUpMfa}>
                  {isSettingUpMfa && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Continue
                </Button>
              </div>
            </form>
          </Form>
        </DialogContent>
      </Dialog>

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
            {mfaSetupData?.recovery_codes?.length ? (
              <div className="space-y-2 text-sm">
                <div className="font-medium">Recovery codes</div>
                <p className="text-muted-foreground">
                  Save these somewhere secure before enabling MFA. Each code can
                  be used once.
                </p>
                <div className="grid grid-cols-2 gap-2 rounded-md bg-muted p-3 font-mono">
                  {mfaSetupData.recovery_codes.map((code) => (
                    <code key={code}>{code}</code>
                  ))}
                </div>
              </div>
            ) : (
              <p className="text-sm text-destructive">
                Recovery codes could not be prepared. Close this dialog and
                restart MFA setup.
              </p>
            )}
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
                  <Button
                    type="submit"
                    disabled={
                      isVerifyingMfa || !mfaSetupData?.recovery_codes?.length
                    }
                  >
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

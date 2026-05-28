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
import { Input } from '@/components/ui/input'
import { zodResolver } from '@hookform/resolvers/zod'
import { Cloud, Loader2, Lock } from 'lucide-react'
import type { ComponentType, SVGProps } from 'react'
import { useForm } from 'react-hook-form'
import { SiAuth0, SiGoogle, SiKeycloak, SiOkta } from 'react-icons/si'
import { Link } from 'react-router-dom'
import { z } from 'zod'

const loginSchema = z.object({
  email: z.string().email('Please enter a valid email address'),
  password: z.string().min(1, 'Password is required'),
})

type LoginFormData = z.infer<typeof loginSchema>

export type OidcProviderOption = {
  /**
   * Stable opaque slug — used as the path parameter for
   * `/api/auth/oidc/login/{slug}`. The integer database ID is
   * intentionally omitted from the public providers endpoint to
   * prevent provider enumeration, so we route by slug here.
   */
  slug: string
  name: string
  /**
   * The provider template the admin selected when creating it (e.g.
   * `keycloak`, `okta`, `google`). Optional because OSS may serve older
   * clients that don't expose it. The login button renders a matching
   * brand logo when present, falling back to a generic Lock icon.
   */
  template?: string
}

/**
 * Pick a brand icon for an SSO provider. Brand marks come from
 * `react-icons/si` (simple-icons) where available — these are widely
 * recognised glyphs and match the provider's own visual identity, so
 * the user sees the logo they expect.
 *
 * Unknown / generic templates fall back to a lucide Lock icon. The
 * fallback is deliberately uniform-weight so the row layout stays
 * stable regardless of which provider mix is configured.
 */
function providerIcon(
  template?: string,
): ComponentType<SVGProps<SVGSVGElement>> {
  switch (template) {
    case 'keycloak':
      return SiKeycloak
    case 'okta':
      return SiOkta
    case 'auth0':
      return SiAuth0
    case 'google':
      return SiGoogle
    case 'azure-ad':
      // No reliable single-color "Azure AD" simple-icons mark since the
      // Entra rebrand. Cloud is the right semantic fallback.
      return Cloud
    case 'generic':
    default:
      return Lock
  }
}

interface LoginFormProps {
  onSubmit: (data: LoginFormData) => Promise<void>
  isLoading?: boolean
  oidcProviders?: OidcProviderOption[]
  /**
   * Hide the email/password fields and the "or" divider. Used by EE when
   * the operator has disabled password login server-side — the form
   * collapses to an SSO-only chooser. When false (default), behaviour is
   * unchanged: email + password are shown, with SSO buttons above if
   * providers are configured.
   *
   * Submitting the form is also blocked when this is false-but-form-empty
   * (no fields rendered = nothing to submit), so we just skip rendering
   * the <form> entirely instead of relying on validation.
   */
  passwordLoginEnabled?: boolean
  /**
   * Show the "Forgot password?" link next to the password field. Only
   * true when the server has an email provider configured (reset links
   * are emailed), so we don't link users to a flow that can't deliver.
   */
  passwordResetAvailable?: boolean
}

export function LoginForm({
  onSubmit,
  isLoading,
  oidcProviders = [],
  passwordLoginEnabled = true,
  passwordResetAvailable = false,
}: LoginFormProps) {
  const form = useForm<LoginFormData>({
    resolver: zodResolver(loginSchema),
    defaultValues: {
      email: '',
      password: '',
    },
  })

  const handleSubmit = async (data: LoginFormData) => {
    await onSubmit(data)
  }

  const startOidcLogin = (slug: string) => {
    window.location.href = `/api/auth/oidc/login/${encodeURIComponent(slug)}`
  }

  return (
    <Card className="w-full max-w-sm">
      <CardHeader>
        <CardTitle className="text-2xl">Login</CardTitle>
        <CardDescription>
          Enter your email and password to access your account
        </CardDescription>
      </CardHeader>
      <CardContent>
        {oidcProviders.length > 0 && (
          <div className="mb-4 space-y-3">
            {oidcProviders.map((provider) => {
              const Icon = providerIcon(provider.template)
              return (
                <Button
                  key={provider.slug}
                  type="button"
                  variant="outline"
                  className="w-full"
                  disabled={isLoading}
                  onClick={() => startOidcLogin(provider.slug)}
                >
                  {/* `aria-hidden` because the visible button text
                      already announces the provider — pairing the icon
                      with the same label would make screen readers
                      say "Keycloak Sign in with Keycloak". */}
                  <Icon className="mr-2 h-4 w-4" aria-hidden="true" />
                  Sign in with {provider.name}
                </Button>
              )
            })}
            {passwordLoginEnabled && (
              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                <div className="h-px flex-1 bg-border" />
                or
                <div className="h-px flex-1 bg-border" />
              </div>
            )}
          </div>
        )}

        {!passwordLoginEnabled && oidcProviders.length === 0 && (
          <p className="text-sm text-muted-foreground">
            Password sign-in is disabled and no SSO provider is configured
            on this server. Contact your administrator.
          </p>
        )}

        {passwordLoginEnabled && (
        <Form {...form}>
          <form
            onSubmit={form.handleSubmit(handleSubmit)}
            className="space-y-4"
          >
            <FormField
              control={form.control}
              name="email"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Email</FormLabel>
                  <FormControl>
                    <Input
                      type="email"
                      placeholder="you@example.com"
                      disabled={isLoading}
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
                  <div className="flex items-center justify-between">
                    <FormLabel>Password</FormLabel>
                    {passwordResetAvailable && (
                      <Link
                        to="/forgot-password"
                        className="text-sm font-medium text-muted-foreground hover:text-foreground"
                      >
                        Forgot password?
                      </Link>
                    )}
                  </div>
                  <FormControl>
                    <Input
                      type="password"
                      placeholder="Enter your password"
                      disabled={isLoading}
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button type="submit" className="w-full" disabled={isLoading}>
              {isLoading ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Signing in...
                </>
              ) : (
                'Sign in'
              )}
            </Button>
          </form>
        </Form>
        )}
      </CardContent>
    </Card>
  )
}

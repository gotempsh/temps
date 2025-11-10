import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { AlertCircle, Key, Lock } from 'lucide-react'
import {
  Control,
  Controller,
  UseFormRegister,
  UseFormSetValue,
} from 'react-hook-form'
import { Alert, AlertDescription } from '@/components/ui/alert'

export interface DockerRegistrySettings {
  enabled: boolean
  registry_url: string | null
  username: string | null
  password: string | null
  tls_verify: boolean
  ca_certificate: string | null
}

interface DockerRegistrySettingsProps {
  control: Control<any>
  register: UseFormRegister<any>
  setValue: UseFormSetValue<any>
  dockerRegistry: DockerRegistrySettings | undefined
}

export function DockerRegistrySettings({
  control,
  register,
  setValue,
  dockerRegistry,
}: DockerRegistrySettingsProps) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Key className="h-5 w-5" />
          Docker Registry Configuration
        </CardTitle>
        <CardDescription>
          Configure an external Docker registry for deployments
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-center justify-between">
          <div className="space-y-0.5">
            <Label htmlFor="docker-registry-enabled">
              Enable External Registry
            </Label>
            <p className="text-sm text-muted-foreground">
              Use a custom Docker registry instead of Docker Hub
            </p>
          </div>
          <Switch
            id="docker-registry-enabled"
            checked={dockerRegistry?.enabled}
            onCheckedChange={(checked) =>
              setValue('docker_registry.enabled', checked, {
                shouldDirty: true,
              })
            }
          />
        </div>

        {dockerRegistry?.enabled && (
          <>
            <Separator />

            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Ensure your registry URL is accessible from the deployment environment.
                Credentials are stored securely and never exposed in API responses.
              </AlertDescription>
            </Alert>

            <div className="space-y-2">
              <Label htmlFor="registry-url">Registry URL</Label>
              <Input
                id="registry-url"
                type="url"
                placeholder="https://registry.example.com"
                {...register('docker_registry.registry_url')}
              />
              <p className="text-sm text-muted-foreground">
                Full URL to the Docker registry (e.g., https://registry.example.com or https://private.azurecr.io)
              </p>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="registry-username">Username</Label>
                <Input
                  id="registry-username"
                  type="text"
                  placeholder="your-username"
                  autoComplete="off"
                  data-lpignore="true"
                  data-1p-ignore="true"
                  {...register('docker_registry.username')}
                />
                <p className="text-sm text-muted-foreground">
                  Registry authentication username
                </p>
              </div>

              <div className="space-y-2">
                <Label htmlFor="registry-password" className="flex items-center gap-2">
                  <Lock className="h-4 w-4" />
                  Password / Token
                </Label>
                <Input
                  id="registry-password"
                  type="password"
                  placeholder="••••••••"
                  autoComplete="new-password"
                  data-lpignore="true"
                  data-1p-ignore="true"
                  {...register('docker_registry.password')}
                />
                <p className="text-sm text-muted-foreground">
                  Registry password or API token. If masked (shown as ••••••••), it's already saved.
                </p>
              </div>
            </div>

            <Separator />

            <div className="flex items-center justify-between">
              <div className="space-y-0.5">
                <Label htmlFor="tls-verify">Verify TLS Certificate</Label>
                <p className="text-sm text-muted-foreground">
                  Validate registry TLS certificate (recommended for production)
                </p>
              </div>
              <Switch
                id="tls-verify"
                checked={dockerRegistry?.tls_verify ?? true}
                onCheckedChange={(checked) =>
                  setValue('docker_registry.tls_verify', checked, {
                    shouldDirty: true,
                  })
                }
              />
            </div>

            {!dockerRegistry?.tls_verify && (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>
                  ⚠️ TLS verification is disabled. This should only be used for testing or self-signed certificates in development.
                </AlertDescription>
              </Alert>
            )}

            <div className="space-y-2">
              <Label htmlFor="ca-certificate">
                CA Certificate (Optional)
              </Label>
              <Textarea
                id="ca-certificate"
                placeholder="-----BEGIN CERTIFICATE-----&#10;MIIDXTCCAkWgAwIBAgIJAJC1/...&#10;-----END CERTIFICATE-----"
                className="font-mono text-sm"
                rows={6}
                {...register('docker_registry.ca_certificate')}
              />
              <p className="text-sm text-muted-foreground">
                PEM-encoded CA certificate for self-signed or private registries. Include the BEGIN/END CERTIFICATE lines.
              </p>
            </div>

            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                <strong>Usage Tips:</strong>
                <ul className="list-disc list-inside mt-2 space-y-1 text-xs">
                  <li><strong>Docker Hub:</strong> Leave URL empty or use https://docker.io</li>
                  <li><strong>Private Registry:</strong> Use full URL (e.g., https://registry.mycompany.com)</li>
                  <li><strong>Self-signed:</strong> Provide CA certificate and optionally disable TLS verification for testing</li>
                  <li><strong>Azure Container Registry:</strong> Use https://[name].azurecr.io with service principal credentials</li>
                  <li><strong>Amazon ECR:</strong> Use registry URL like https://[account-id].dkr.ecr.[region].amazonaws.com with AWS credentials</li>
                </ul>
              </AlertDescription>
            </Alert>
          </>
        )}

        {!dockerRegistry?.enabled && (
          <div className="rounded-lg border border-dashed p-4 text-center text-sm text-muted-foreground">
            External Docker registry is currently disabled. Enable it to use a custom registry for deployments.
          </div>
        )}
      </CardContent>
    </Card>
  )
}

import { test, expect, describe } from 'bun:test'
import { resolveDnsProviderCredentials } from './index.js'
import type { DnsProviderType } from '../../api/types.gen.js'

describe('resolveDnsProviderCredentials', () => {
  describe('cloudflare', () => {
    test('builds credentials from the api token alone', () => {
      expect(resolveDnsProviderCredentials('cloudflare', { apiToken: 'tok' })).toEqual({
        type: 'cloudflare',
        api_token: 'tok',
      })
    })

    test('includes the optional account id only when supplied', () => {
      expect(
        resolveDnsProviderCredentials('cloudflare', { apiToken: 'tok', accountId: 'acct' }),
      ).toEqual({ type: 'cloudflare', api_token: 'tok', account_id: 'acct' })
    })

    test('defers to interactive prompts when no flags and no --yes', () => {
      expect(resolveDnsProviderCredentials('cloudflare', {})).toBeUndefined()
    })

    test('throws under --yes so automation never silently prompts', () => {
      expect(() => resolveDnsProviderCredentials('cloudflare', { yes: true })).toThrow(
        '--api-token is required for Cloudflare when using --yes flag',
      )
    })
  })

  describe('route53', () => {
    test('defaults region to us-east-1 when not given', () => {
      expect(
        resolveDnsProviderCredentials('route53', {
          accessKeyId: 'AKIA',
          secretAccessKey: 'shh',
        }),
      ).toEqual({
        type: 'route53',
        access_key_id: 'AKIA',
        secret_access_key: 'shh',
        region: 'us-east-1',
      })
    })

    test('uses an explicit region over the default', () => {
      expect(
        resolveDnsProviderCredentials('route53', {
          accessKeyId: 'AKIA',
          secretAccessKey: 'shh',
          region: 'eu-west-1',
        }),
      ).toEqual({
        type: 'route53',
        access_key_id: 'AKIA',
        secret_access_key: 'shh',
        region: 'eu-west-1',
      })
    })

    test('requires both key fields under --yes, not just one', () => {
      expect(() =>
        resolveDnsProviderCredentials('route53', { accessKeyId: 'AKIA', yes: true }),
      ).toThrow('--access-key-id and --secret-access-key are required for Route53 when using --yes flag')
    })
  })

  describe('digitalocean', () => {
    test('builds credentials from the api token', () => {
      expect(resolveDnsProviderCredentials('digitalocean', { apiToken: 'do-tok' })).toEqual({
        type: 'digitalocean',
        api_token: 'do-tok',
      })
    })

    test('throws under --yes without a token', () => {
      expect(() => resolveDnsProviderCredentials('digitalocean', { yes: true })).toThrow(
        '--api-token is required for DigitalOcean when using --yes flag',
      )
    })
  })

  describe('namecheap', () => {
    const full = { apiUser: 'user', apiKey: 'key', username: 'uname', clientIp: '1.2.3.4' }

    test('requires all four fields before building credentials', () => {
      expect(resolveDnsProviderCredentials('namecheap', full)).toEqual({
        type: 'namecheap',
        api_user: 'user',
        api_key: 'key',
        username: 'uname',
        client_ip: '1.2.3.4',
      })
    })

    test('a single missing field under --yes names every required flag', () => {
      const { clientIp: _clientIp, ...partial } = full
      expect(() =>
        resolveDnsProviderCredentials('namecheap', { ...partial, yes: true }),
      ).toThrow(
        '--api-user, --api-key, --username, and --client-ip are required for Namecheap when using --yes flag',
      )
    })
  })

  describe('gcp', () => {
    const full = {
      projectId: 'proj',
      serviceAccountEmail: 'sa@proj.iam.gserviceaccount.com',
      privateKeyId: 'kid',
      privateKey: '-----BEGIN PRIVATE KEY-----',
    }

    test('maps flags to the service-account credential shape', () => {
      expect(resolveDnsProviderCredentials('gcp', full)).toEqual({
        type: 'gcp',
        project_id: 'proj',
        service_account_email: 'sa@proj.iam.gserviceaccount.com',
        private_key_id: 'kid',
        private_key: '-----BEGIN PRIVATE KEY-----',
      })
    })

    test('throws under --yes when any field is missing', () => {
      const { privateKey: _privateKey, ...partial } = full
      expect(() => resolveDnsProviderCredentials('gcp', { ...partial, yes: true })).toThrow(
        '--project-id, --service-account-email, --private-key-id, and --private-key are required for GCP when using --yes flag',
      )
    })
  })

  describe('azure', () => {
    const full = {
      tenantId: 'tenant',
      clientId: 'client',
      clientSecret: 'secret',
      subscriptionId: 'sub',
      resourceGroup: 'rg',
    }

    test('maps flags to the service-principal credential shape', () => {
      expect(resolveDnsProviderCredentials('azure', full)).toEqual({
        type: 'azure',
        tenant_id: 'tenant',
        client_id: 'client',
        client_secret: 'secret',
        subscription_id: 'sub',
        resource_group: 'rg',
      })
    })

    test('throws under --yes when any field is missing', () => {
      const { resourceGroup: _resourceGroup, ...partial } = full
      expect(() => resolveDnsProviderCredentials('azure', { ...partial, yes: true })).toThrow(
        '--tenant-id, --client-id, --client-secret, --subscription-id, and --resource-group are required for Azure when using --yes flag',
      )
    })
  })

  describe('manual', () => {
    test('never requires flags, with or without --yes', () => {
      expect(resolveDnsProviderCredentials('manual', {})).toEqual({ type: 'manual' })
      expect(resolveDnsProviderCredentials('manual', { yes: true })).toEqual({ type: 'manual' })
    })
  })

  describe('pebble', () => {
    test('builds credentials from the management url', () => {
      expect(
        resolveDnsProviderCredentials('pebble', { managementUrl: 'http://localhost:8055' }),
      ).toEqual({ type: 'pebble', management_url: 'http://localhost:8055' })
    })

    test('defers to interactive prompt without --yes', () => {
      expect(resolveDnsProviderCredentials('pebble', {})).toBeUndefined()
    })

    test('throws under --yes without a management url', () => {
      expect(() => resolveDnsProviderCredentials('pebble', { yes: true })).toThrow(
        '--management-url is required for Pebble when using --yes flag',
      )
    })
  })

  test('rejects a provider type outside the supported set', () => {
    expect(() =>
      resolveDnsProviderCredentials('bogus' as DnsProviderType, {}),
    ).toThrow('Unsupported provider type: bogus')
  })
})

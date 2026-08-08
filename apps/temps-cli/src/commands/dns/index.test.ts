import { test, expect, describe } from 'bun:test'
import {
  cloudflareCredentials,
  route53Credentials,
  digitalOceanCredentials,
  namecheapCredentials,
  gcpCredentials,
  azureCredentials,
} from './index.js'

describe('cloudflareCredentials', () => {
  test('omits account_id when not provided', () => {
    expect(cloudflareCredentials('tok')).toEqual({ type: 'cloudflare', api_token: 'tok' })
  })

  test('includes account_id when provided', () => {
    expect(cloudflareCredentials('tok', 'acct-1')).toEqual({
      type: 'cloudflare',
      api_token: 'tok',
      account_id: 'acct-1',
    })
  })

  test('treats an empty account_id the same as omitted, not an empty field', () => {
    expect(cloudflareCredentials('tok', '')).toEqual({ type: 'cloudflare', api_token: 'tok' })
  })
})

describe('route53Credentials', () => {
  test('maps fields to the API snake_case shape', () => {
    expect(route53Credentials('AKIA123', 'secret', 'eu-west-1')).toEqual({
      type: 'route53',
      access_key_id: 'AKIA123',
      secret_access_key: 'secret',
      region: 'eu-west-1',
    })
  })
})

describe('digitalOceanCredentials', () => {
  test('carries only the api token', () => {
    expect(digitalOceanCredentials('do-token')).toEqual({
      type: 'digitalocean',
      api_token: 'do-token',
    })
  })
})

describe('namecheapCredentials', () => {
  test('maps all four required fields', () => {
    expect(namecheapCredentials('user', 'key', 'uname', '1.2.3.4')).toEqual({
      type: 'namecheap',
      api_user: 'user',
      api_key: 'key',
      username: 'uname',
      client_ip: '1.2.3.4',
    })
  })
})

describe('gcpCredentials', () => {
  test('maps service account fields', () => {
    expect(gcpCredentials('proj', 'sa@proj.iam.gserviceaccount.com', 'kid', 'PRIVATE_KEY')).toEqual({
      type: 'gcp',
      project_id: 'proj',
      service_account_email: 'sa@proj.iam.gserviceaccount.com',
      private_key_id: 'kid',
      private_key: 'PRIVATE_KEY',
    })
  })
})

describe('azureCredentials', () => {
  test('maps service principal fields', () => {
    expect(azureCredentials('tenant', 'client', 'secret', 'sub', 'rg')).toEqual({
      type: 'azure',
      tenant_id: 'tenant',
      client_id: 'client',
      client_secret: 'secret',
      subscription_id: 'sub',
      resource_group: 'rg',
    })
  })
})

import { test, expect, describe } from 'bun:test'
import { formatFileSize, getContentType, isValidArchive } from './deploy-static.js'

describe('isValidArchive', () => {
  test('accepts .tar.gz as a two-part extension', () => {
    expect(isValidArchive('/tmp/site.tar.gz')).toBe(true)
  })

  test('accepts .tgz and .zip', () => {
    expect(isValidArchive('/tmp/site.tgz')).toBe(true)
    expect(isValidArchive('/tmp/site.zip')).toBe(true)
  })

  test('is case-insensitive', () => {
    expect(isValidArchive('/tmp/SITE.TAR.GZ')).toBe(true)
    expect(isValidArchive('/tmp/SITE.ZIP')).toBe(true)
  })

  test('rejects anything else, including bare .gz', () => {
    // A bare .gz (not .tar.gz) is not something the server can unpack — this
    // guards against silently uploading an unsupported bundle.
    expect(isValidArchive('/tmp/site.gz')).toBe(false)
    expect(isValidArchive('/tmp/site.rar')).toBe(false)
    expect(isValidArchive('/tmp/site')).toBe(false)
  })
})

describe('getContentType', () => {
  test('maps tar.gz/tgz to gzip', () => {
    expect(getContentType('/tmp/site.tar.gz')).toBe('application/gzip')
    expect(getContentType('/tmp/site.tgz')).toBe('application/gzip')
  })

  test('maps .zip to zip', () => {
    expect(getContentType('/tmp/site.zip')).toBe('application/zip')
  })

  test('falls back to octet-stream for anything unrecognized', () => {
    expect(getContentType('/tmp/site.rar')).toBe('application/octet-stream')
  })
})

describe('formatFileSize', () => {
  test('renders bytes under 1 KB as whole bytes', () => {
    expect(formatFileSize(512)).toBe('512 B')
  })

  test('renders KB with one decimal', () => {
    expect(formatFileSize(2048)).toBe('2.0 KB')
  })

  test('renders MB with two decimals', () => {
    expect(formatFileSize(1024 * 1024 * 3)).toBe('3.00 MB')
  })
})

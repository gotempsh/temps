// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { SettingsSection, Status } from '@temps-sdk/ds'
import { ShieldAlert } from 'lucide-react'
import { readCspReport } from '@/lib/csp-report'

/** Native browser-report detail; browsers need not submit SDK metadata. */
export function CspReportDetails({ data }: { data: unknown }) {
  const fields = readCspReport(data)
  if (!fields) return null
  const display = (key: string) => {
    const value = fields[key]
    return typeof value === 'string' || typeof value === 'number'
      ? String(value)
      : '—'
  }
  const reportOnly = fields.disposition === 'report'
  return (
    <SettingsSection
      title="Content security policy"
      icon={ShieldAlert}
      defaultOpen
    >
      <div className="space-y-4">
        <Status
          tone={reportOnly ? 'idle' : 'warn'}
          label={reportOnly ? 'Report-only policy' : 'Policy violation'}
        />
        <dl className="grid grid-cols-1 gap-4 text-sm sm:grid-cols-2">
          {[
            ['Directive', 'effective_directive'],
            ['Blocked resource', 'blocked_uri'],
            ['Document', 'document_uri'],
            ['Source file', 'source_file'],
            ['Line', 'line_number'],
            ['Column', 'column_number'],
          ].map(([label, key]) => (
            <div key={key} className="min-w-0 space-y-1">
              <dt className="text-xs text-muted-foreground">{label}</dt>
              <dd className="break-all font-mono text-xs">{display(key)}</dd>
            </div>
          ))}
        </dl>
        <div className="space-y-1">
          <p className="text-xs text-muted-foreground">Original policy</p>
          <pre className="whitespace-pre-wrap break-all font-mono text-xs">
            {display('original_policy')}
          </pre>
        </div>
        {typeof fields.script_sample === 'string' && fields.script_sample && (
          <div className="space-y-1">
            <p className="text-xs text-muted-foreground">Reported sample</p>
            <pre className="whitespace-pre-wrap break-all font-mono text-xs">
              {fields.script_sample}
            </pre>
          </div>
        )}
      </div>
    </SettingsSection>
  )
}

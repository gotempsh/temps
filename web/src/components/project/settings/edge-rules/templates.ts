// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { newCondition, newGroup } from './expression'
import type { Condition, EdgeRuleDraft, FieldId } from './types'

export interface RuleTemplate {
  id: string
  title: string
  description: string
  build: () => EdgeRuleDraft
}

const cond = (
  field: FieldId,
  fields: Partial<Omit<Condition, 'kind' | 'id' | 'field'>>
): Condition => ({ ...newCondition(field), ...fields })

/** Examples offered when a project has no rules. Every template starts in
 * log-only mode so it can be watched before it blocks anything. */
export const RULE_TEMPLATES: RuleTemplate[] = [
  {
    id: 'cms-probes',
    title: 'Block CMS and secret-file probes',
    description:
      'Paths such as /wp-login.php, /.env and /.git/config that scanners try on every site.',
    build: () => ({
      name: 'Block CMS and secret-file probes',
      enabled: true,
      mode: 'log_only',
      environmentId: null,
      expression: newGroup('any', [
        cond('path', { op: 'glob', value: '/wp-*', caseInsensitive: true }),
        cond('path', { op: 'glob', value: '*.php', caseInsensitive: true }),
        cond('path', { op: 'glob', value: '/.env*' }),
        cond('path', { op: 'glob', value: '/.git*' }),
      ]),
      action: { type: 'block', status: 404, body: '' },
    }),
  },
  {
    id: 'admin-office',
    title: 'Restrict /admin to your office network',
    description:
      'Allow the admin area from one IP range and block it for everyone else.',
    build: () => ({
      name: 'Restrict /admin to office network',
      enabled: true,
      mode: 'log_only',
      environmentId: null,
      expression: newGroup('all', [
        cond('path', { op: 'glob', value: '/admin*', caseInsensitive: true }),
        newGroup('none', [
          cond('ip', { op: 'in_cidr', value: '203.0.113.0/24' }),
        ]),
      ]),
      action: { type: 'block', status: 403, body: '' },
    }),
  },
  {
    id: 'hosting-challenge',
    title: 'Challenge traffic from hosting networks',
    description:
      'Most automated traffic comes from cloud providers. Real visitors rarely do.',
    build: () => ({
      name: 'Challenge hosting-provider traffic',
      enabled: true,
      mode: 'log_only',
      environmentId: null,
      expression: newGroup('all', [
        cond('is_hosting_provider', { op: 'eq', value: 'true' }),
        newGroup('none', [
          cond('bot_class', { op: 'in', value: 'search_engine' }),
        ]),
      ]),
      action: { type: 'challenge' },
    }),
  },
  {
    id: 'docs-redirect',
    title: 'Redirect /docs to a docs site',
    description: 'Send /docs/* to another host, keeping the path and query.',
    build: () => ({
      name: 'Redirect /docs to docs site',
      enabled: true,
      mode: 'log_only',
      environmentId: null,
      expression: newGroup('all', [
        cond('path', { op: 'glob', value: '/docs*' }),
      ]),
      action: {
        type: 'redirect',
        status: 308,
        target: 'https://docs.example.com{path}?{query}',
      },
    }),
  },
]

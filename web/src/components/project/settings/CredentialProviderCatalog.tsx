// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { CredentialProviderMark } from '@temps-sdk/ds'
import type { ProviderPreset } from '@/api/client'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'

function filterCredentialProviders(providers: ProviderPreset[], query: string) {
  const words = query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean)
  return providers
    .filter((provider) => {
      const text =
        `${provider.id} ${provider.name} ${provider.description} ${provider.spec.url} ${provider.automatic ? 'automatic daily' : 'configure manually'}`.toLocaleLowerCase()
      return words.every((word) => text.includes(word))
    })
    .sort((a, b) => a.name.localeCompare(b.name))
}

export function CredentialProviderCatalog({
  providers,
  selectedId,
  onSelect,
}: {
  providers: ProviderPreset[]
  selectedId: string
  onSelect: (id: string) => void
}) {
  const [query, setQuery] = useState('')
  const [page, setPage] = useState(1)
  const filtered = filterCredentialProviders(providers, query)
  const pageSize = 6
  const currentPage = Math.min(
    page,
    Math.max(1, Math.ceil(filtered.length / pageSize))
  )
  return (
    <section aria-label="Credential provider catalog" className="space-y-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="text-base font-medium">
            Supported providers{' '}
            <span className="text-sm text-muted-foreground">
              ({providers.length})
            </span>
          </h3>
          <p className="mt-1 text-sm text-muted-foreground">
            {providers.filter((p) => p.automatic).length} support automatic
            daily checks for recognized tokens. Other templates need explicit
            configuration.
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onSelect('custom')}
        >
          Custom HTTP check
        </Button>
      </div>
      <Input
        aria-label="Search credential providers"
        placeholder="Search providers, endpoints, or capabilities…"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value)
          setPage(1)
        }}
      />
      <p className="text-xs text-muted-foreground">
        Checks verify access to the listed endpoint. They do not verify credits
        or expiration unless stated. Choosing a template does not send a
        credential. Manual templates can use non-secret variables; write-only
        secrets require a recognized automatic provider.
      </p>
      <div className="overflow-hidden rounded-lg border">
        {filtered.length === 0 ? (
          <div role="status" className="p-6 text-sm text-muted-foreground">
            No matching providers. Try another search or configure a custom HTTP
            check.
          </div>
        ) : (
          <ul className="divide-y">
            {filtered
              .slice((currentPage - 1) * pageSize, currentPage * pageSize)
              .map((provider) => (
                <li
                  key={provider.id}
                  className="flex items-start gap-3 px-4 py-3"
                >
                  <CredentialProviderMark provider={provider.id} />
                  <div className="min-w-0 flex-1 space-y-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm font-medium">
                        {provider.name}
                      </span>
                      <Badge variant="secondary">
                        {provider.automatic
                          ? 'Automatic'
                          : 'Configure manually'}
                      </Badge>
                    </div>
                    <p className="text-sm text-muted-foreground">
                      {provider.description}
                    </p>
                    <p className="break-all font-mono text-xs text-muted-foreground">
                      GET {provider.spec.url}
                    </p>
                    <a
                      href={provider.documentation_url}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-xs underline underline-offset-4"
                    >
                      Provider documentation
                      <span className="sr-only"> for {provider.name}</span>
                    </a>
                  </div>
                  <Button
                    type="button"
                    size="sm"
                    variant={
                      selectedId === provider.id ? 'secondary' : 'outline'
                    }
                    aria-pressed={selectedId === provider.id}
                    aria-label={`Use ${provider.name} template`}
                    onClick={() => onSelect(provider.id)}
                  >
                    {selectedId === provider.id ? 'Selected' : 'Use'}
                  </Button>
                </li>
              ))}
          </ul>
        )}
      </div>
      <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span role="status">
          {filtered.length} {filtered.length === 1 ? 'provider' : 'providers'}
          {filtered.length > pageSize
            ? ` · Page ${currentPage} of ${Math.ceil(filtered.length / pageSize)}`
            : ''}
        </span>
        {filtered.length > pageSize && (
          <div className="flex gap-2">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={currentPage === 1}
              onClick={() => setPage(currentPage - 1)}
            >
              Previous providers
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={currentPage * pageSize >= filtered.length}
              onClick={() => setPage(currentPage + 1)}
            >
              Next providers
            </Button>
          </div>
        )}
      </div>
    </section>
  )
}

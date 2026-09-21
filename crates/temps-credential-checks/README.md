# Credential checks

Provider-independent credential detection and verification. This crate has no
Temps database, authentication, scheduler, or notification dependencies.

## Extension points

- `CredentialDetector`: identify candidates locally from a variable name and value.
  Returns rule IDs and evidence, never plaintext or matched substrings.
- `CredentialVerifier`: verify an explicitly selected credential using an injected
  `HttpCheckTransport`. Implement a custom verifier for multi-step protocols.
- `HttpCheckTransport`: execute a request with host-controlled destination policy,
  timeouts, response limits and TLS. Tests inject a mock; Temps supplies a DNS-pinned
  transport which rejects private destinations, proxies and redirects.

`HttpCredentialVerifier` implements the common declarative case: GET/HEAD,
credential injection into a header, accepted successful status codes, expiration
from a header or JSON pointer, and numeric warning/critical thresholds. Unsupported
or unavailable data is `unknown`, never an inferred healthy balance.

## Catalog and verification coverage

`CatalogDetector::bundled()` compiles the pinned Gitleaks expressions once at startup.
The upstream catalog has 222 rules; rules without a value expression (such as
path-only detection) do not apply to environment variables and are omitted.
Variable-name aliases supplement these patterns for the initial providers.
See [catalog provenance](catalog/SOURCE.md) and the included MIT license.

These are candidate hints, not a full Gitleaks repository scanner: file allowlists,
commit exclusions, and multi-file context do not apply here. Detection never sends
a secret anywhere. `automatic_preset` separately selects a reviewed public destination
when there is exactly one supported issuer. A generic `API_KEY`
match cannot establish who issued it.

Initial reviewed HTTP presets: GitHub personal tokens, GitLab personal tokens,
OpenAI and Anthropic. Other providers use a custom HTTP recipe until a reviewed
preset exists. GitLab's preset checks expiry; model-list checks establish API
access only, not prepaid credit availability. GitHub App installation tokens need
a different endpoint from the personal-token preset.

## Automatic selection

The automatic policy supports GitHub personal/OAuth tokens, OpenAI, and Anthropic.
Conflicting provider hints, GitHub installation/refresh tokens, and Anthropic admin
keys are not automatically verified. GitLab and Temps tokens require an explicit
endpoint because the token format does not identify a self-hosted issuer.

## Result semantics

- `healthy`: configured response rules passed.
- `warning`: insufficient access, expiration threshold, or numeric warning.
- `error`: rejected authentication, expired credential, or critical threshold.
- `unknown`: timeout, rate limit, provider outage, missing field or unreadable value.

Findings have stable codes. `fingerprint()` ignores changing values, so a daily
scheduler can deduplicate alerts while still alerting when a 30-day expiry warning
moves to the 7-day threshold. Provider response bodies and credentials are never
included in findings. The owning application handles encryption and persistence.

## Validation

`cargo test --lib -p temps-credential-checks`

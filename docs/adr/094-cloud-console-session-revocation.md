# ADR-094: Temps Cloud console sessions end when Cloud revokes the user

**Status:** Accepted
**Date:** 2026-11-08
**Related:** ADR-039 (the management channel is advisory), ADR-045 (the
Cloud-managed OIDC provider), OpenID Connect Back-Channel Logout 1.0.

## Context

An instance with the Cloud-managed OIDC provider (`oidc_providers.managed_by_cloud`)
signs people in with Temps Cloud. Cloud's ID token lives two minutes, but the
instance then creates its own session, which lasts seven days. If Cloud
revoked a user's access, or their role on this instance, that session kept
working until it expired.

## Decision

The instance accepts two signals from Cloud. If Cloud stays silent, it also
stops trusting its Cloud sessions after a fixed time.

1. **Signed logout push.** With an auth plugin registered, the management
   channel's `Hello` advertises `console_session_logout`. Cloud may then send
   `console_logout {logout_token}`. The token is an OpenID Connect
   Back-Channel Logout token, and the instance verifies it against the managed
   provider's own JWKS. The JWKS is refreshed once if the key id is unknown.
   The token must have:

   - JOSE `typ` `logout+jwt` and `alg` RS256;
   - the provider's issuer;
   - this instance's `client_id` in `aud`;
   - the back-channel logout event;
   - no `nonce`;
   - a subject;
   - an unexpired `exp`;
   - an `iat` no more than five minutes in the future.

   The instance then ends that subject's Cloud-issued sessions and the user's
   ingress login grants, in one transaction, and answers
   `console_logout_ack {jti, sessions_ended}`. A token it refuses is answered
   with `console_logout_rejected {jti, reason}`.

   This is the only management-channel message the instance acts on. Acting on
   it is safe for two reasons: the only action is removing access, and the
   authority is the identity provider's signature, not the channel. A token
   that is replayed or relayed by a compromised intermediary can only log out
   a user the identity provider already chose to log out.

2. **Re-check.** Every 60 seconds, if Cloud-issued sessions exist, the
   instance posts their subjects to `{issuer}/console/session-status`. It
   authenticates with HTTP Basic, using its client id and secret, and sends at
   most 50 subjects per request. The reply, `{"active": [...]}`, lists the
   subjects that may stay. Every other subject loses its sessions. A `401`
   ends every Cloud-issued session.

3. **Fail closed.** A network error, timeout, 5xx or 429 changes nothing at
   first. But once no check has succeeded for 10 minutes, every Cloud-issued
   session ends. Signing in again needs Cloud anyway.

Only sessions whose origin is `oidc:<managed provider id>` are affected.
Password sessions, other OIDC providers, MFA state and every workload are
untouched. On an instance without the managed provider, each check is a single
local query that finds nothing to do, and the instance advertises the
capability but never receives the frame.

**Bound:** with the channel up, a revoked user loses the console within
seconds. When Cloud's API is reachable, it is within 90 seconds. In all cases
it is within 10 minutes.

## Consequences

- A Cloud API outage longer than 10 minutes signs out everyone who signed in
  with Temps Cloud. Local logins keep working.
- Old Cloud backends never send the frame. The re-check endpoint is optional:
  until a backend serves it, its absence reads as "unavailable", so Cloud
  sessions are bounded at 10 minutes. Instances enabling the managed provider
  require a backend that serves it.

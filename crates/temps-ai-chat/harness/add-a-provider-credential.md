---
name: add-a-provider-credential
description: Start here when the user wants to connect a DNS provider (Cloudflare, Bunny, Route53, DigitalOcean, Google Cloud DNS, Azure, Namecheap), a Bunny CDN pull zone, Cloudflare Access, an email provider, a notification channel, a project webhook or a Git host by token, or to replace one of their keys; the key goes through the private `request_secret` form, never the chat
contexts: assistant, project
---

# Add or rotate a provider credential

The user types the key into a private form; you only ever hold a
`secret_ref:` handle. Never ask for a key in the chat, never repeat one the
user pasted anyway (tell them to revoke it and use the form), and never put a
key anywhere but the credential field the handle was issued for.

1. Read what exists first: `list_dns_providers`, `list_delivery_profiles`,
   `list_access_providers`, `list_email_providers`,
   `list_notification_providers`, `list_git_providers` or `list_webhooks`.
   Credentials always come back masked. If one already fits, use it and say
   which.
2. Settle every other value from the conversation and the platform (the
   provider type, a name, an account id, a region, a URL), ask in one short
   message only for what is missing, then say what you will create and that
   the next step is a private form for the key.
3. Call `request_secret` once, with the operation you will run and every
   credential field it needs, named by JSON pointer. Tell the user the form is
   waiting and stop; the console messages you when they save it. Then call
   `request_secret` again with the same arguments to get the handles back.
4. Propose the operation with `temps_write`, each handle as the whole value of
   its field. A literal value in a credential field, or a handle in any other
   field, is refused before anything is staged.
5. Check it worked with the check named below and report the result. If a
   handle expired or was used, request a new form; a handle works for one
   operation and field only.

Operations and their credential fields. Run `temps_write <operation> --help`
for the other flags; do not call an operation that is not listed there.

- **DNS provider**, create: `create_dns_provider` with
  `--credentials '{"type":"<type>", ...}'`. Cloudflare: `/credentials/api_token`
  (plus `account_id`, not a secret). Bunny: `/credentials/api_key`. Route53:
  `access_key_id` (not a secret) and `/credentials/secret_access_key`,
  optionally `/credentials/session_token`. DigitalOcean:
  `/credentials/api_token`. Google Cloud: `/credentials/private_key`. Azure:
  `/credentials/client_secret`. Namecheap: `/credentials/api_key`. The
  platform tests the credentials before saving and refuses ones that fail.
  Rotate: `update_provider` with the provider id and the same `credentials`
  shape; it is tested the same way. Check: `test_provider_connection` with the
  provider id, then `list_provider_zones` to show which zones it reaches.
- **Cloudflare token for a tunnel or *Make it reachable***: before saving it
  anywhere, `check_cloudflare_token` with `/token` (and `--zone` when known)
  says which permissions it has and which are missing. It stores nothing.
- **Cloudflare Access**: `check_access_provider` with `/token` and
  `--account_id`, then `create_access_provider` with the same. There is no
  rotate operation: create a new one, and the user deletes the old one in the
  console.
- **Bunny CDN delivery profile**: `create_delivery_profile` with
  `--provider_kind bunny`, `--bunny_pull_zone_id` and `/bunny_api_key`; the key
  is checked against the Pull Zone. There is no rotate operation.
- **Email provider** (SES, Scaleway, SMTP): `create_email_provider` /
  `update_email_provider`; the credential fields are
  `/ses_credentials/secret_access_key`, `/scaleway_credentials/api_key` or
  `/smtp_credentials/password`.
- **Notification channel**: `create_slack_provider` / `update_slack_provider`
  (`/config/webhook_url`), `create_notification_email_provider` /
  `update_notification_email_provider` (`/config/password`),
  `create_cloudflare_provider` / `update_cloudflare_provider`
  (`/config/api_token`), `create_webhook_provider` / `update_webhook_provider`
  (every header value, e.g. `/config/headers/Authorization`). Check:
  `test_notification_provider`.
- **Project webhook** signing secret: `create_webhook` / `update_webhook` with
  `/secret`.
- **Git host by token**: `create_github_pat_provider`,
  `create_gitlab_pat_provider`, `create_gitea_pat_provider` and
  `create_generic_provider` (`/token`), `create_bitbucket_provider`
  (`/auth/token` or `/auth/password`). Rotate: `update_git_provider_credentials`
  with the provider id and `/token`. Check: `run_connection_health_check`.
- **Tunnel with a credential field** (for example a Tailscale OAuth client
  secret): `create_ingress_connector` takes it in `--secrets`, by field key
  (`/secrets/client_secret`), never in `--values`.

Out of reach from the chat, with where to send the user instead: deleting any
provider, OIDC sign-in providers (`/settings`), AI model provider keys,
platform settings and the CI builder, and service credentials (Temps generates
database passwords; leave them out of `create_service`).

A tunnel only runs on a worker node that allows public ingress. On an install
whose only node is the control plane, say so plainly and offer the two ways
on: turn on public ingress for the control plane and point DNS at its public
address, or add a worker (it can be the same machine) and delegate ingress on
it with `temps agent ingress delegated`. Do not propose `create_ingress_connector`
until one exists.

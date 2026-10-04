# Security Policy

## Supported Versions

Temps follows [Semantic Versioning](https://semver.org/). From 1.0 onwards,
security fixes are provided for:

| Version                              | Supported                                                                 |
|--------------------------------------|---------------------------------------------------------------------------|
| Latest minor release (`1.N.x`)       | :white_check_mark: All fixes, including security fixes                    |
| Previous minor release (`1.(N-1).x`) | :white_check_mark: Security fixes only, for 6 months after `1.N.0` ships  |
| Older minor releases                 | :x:                                                                       |
| Pre-1.0 releases (`0.x`, betas)      | :x: Upgrade to the latest 1.x release                                     |

Security fixes are released as patch versions (for example `1.4.2`) of every
supported minor line. Upgrading within a minor line never requires a
configuration change. See the
[upgrade guide](https://temps.sh/docs/upgrade) for moving between releases.

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via email to **security@temps.sh**.

You should receive a response within 48 hours. If for some reason you do not, please follow up via email to ensure we received your original message.

Please include the following information in your report:

- Type of issue (e.g., buffer overflow, SQL injection, cross-site scripting, etc.)
- Full paths of source file(s) related to the issue
- The location of the affected source code (tag/branch/commit or direct URL)
- Any special configuration required to reproduce the issue
- Step-by-step instructions to reproduce the issue
- Proof-of-concept or exploit code (if possible)
- Impact of the issue, including how an attacker might exploit it

## Disclosure Policy

We follow coordinated disclosure. The timeline below starts when we receive
your report:

| Step | Target |
|------|--------|
| Acknowledge receipt | Within 48 hours |
| Confirm the issue, assess severity, and share an estimated fix timeline | Within 7 days |
| Release a fix for critical and high severity issues | Within 30 days |
| Release a fix for medium and low severity issues | Within 90 days |
| Publish a security advisory | When the fix is released |

- We will keep you informed of progress and notify you when the fix is released.
- If a fix needs more time than the targets above, we will agree a new date with you before it passes.
- We publish a GitHub security advisory for each fixed vulnerability, requesting a CVE where applicable, and list the affected and fixed versions.
- We credit reporters in the advisory unless you prefer to remain anonymous.
- Please give us the chance to release a fix before you disclose the issue publicly. If no fix is released within 90 days of your report, you may disclose it after giving us notice.

## Security Best Practices for Self-Hosters

When deploying Temps, please ensure:

1. **Use HTTPS** — Always configure TLS certificates for your deployment.
2. **Strong passwords** — Use strong passwords for admin accounts and database connections.
3. **Firewall rules** — Restrict access to management ports at the network/OS level, and use the [Admin Listener](https://temps.sh/docs/admin-listener) to bind the admin/dashboard surface to a private interface with CIDR + Host allowlists.
4. **Keep updated** — Run `temps upgrade` regularly to get the latest security patches.
5. **Database security** — Use strong PostgreSQL credentials and restrict network access.
6. **API keys** — Rotate API keys periodically and use the minimum required permissions.

## Scope

The following are in scope:

- The Temps server binary (`temps`)
- The web UI
- The reverse proxy (Pingora-based)
- Authentication and authorization systems
- API endpoints
- SDKs (`@temps-sdk/*`)

The following are out of scope:

- Third-party dependencies (report these to the respective maintainers)
- Issues in applications deployed on Temps (report to the application owners)
- Social engineering attacks
- Denial of service attacks

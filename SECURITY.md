# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| latest  | :white_check_mark: |

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

- We will acknowledge receipt of your vulnerability report within 48 hours.
- We will provide an estimated timeline for a fix within 7 days.
- We will notify you when the vulnerability is fixed.
- We will publicly disclose the vulnerability after a fix is available, crediting you (unless you prefer to remain anonymous).

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

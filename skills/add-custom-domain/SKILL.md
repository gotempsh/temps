---
name: add-custom-domain
description: |
  Configure custom domains for Temps deployments with automatic SSL/TLS certificates via Let's Encrypt. Supports HTTP-01 and DNS-01 challenges, wildcard domains, and Cloudflare DNS integration. Use when the user wants to: (1) Add a custom domain to their Temps app, (2) Set up SSL certificates, (3) Configure DNS for Temps, (4) Add a wildcard domain, (5) Set up HTTPS for their deployment, (6) Configure Cloudflare with Temps. Triggers: "custom domain", "add domain", "ssl certificate", "https setup", "wildcard domain", "dns configuration", "cloudflare temps".
---

# Add Custom Domain

Configure custom domains with automatic SSL/TLS certificates.

## Quick Setup

### 1. Add Domain in Dashboard

1. Go to Project > Settings > Domains
2. Click "Add Domain"
3. Enter your domain (e.g., `app.example.com`)

### 2. Configure DNS

Add a CNAME record pointing to your Temps deployment:

| Type | Name | Value |
|------|------|-------|
| CNAME | app | your-project.temps.io |

For apex domains (example.com without subdomain), use an A record:

| Type | Name | Value |
|------|------|-------|
| A | @ | [Temps IP address] |

### 3. Verify & Get Certificate

Temps automatically:
1. Verifies DNS configuration
2. Provisions Let's Encrypt certificate
3. Enables HTTPS

## Challenge Types

### HTTP-01 (Default)

Used for standard domains. Temps handles automatically.

**Requirements:**
- Domain must point to Temps
- Port 80 must be accessible

### DNS-01 (Wildcard & Private)

Required for wildcard domains (`*.example.com`).

**Setup with Cloudflare:**

1. Add Cloudflare API token in Temps:
   - Go to Settings > DNS Providers
   - Add Cloudflare with Zone API token

2. Add wildcard domain:
   ```
   *.example.com
   ```

3. Temps creates DNS TXT record automatically

## Wildcard Domains

For `*.example.com` to match `app.example.com`, `api.example.com`, etc:

1. **Requires DNS-01 challenge** (HTTP-01 doesn't support wildcards)
2. **Requires DNS provider integration** (Cloudflare supported)

### Cloudflare Setup

1. **Create API Token:**
   - Go to Cloudflare Dashboard > Profile > API Tokens
   - Create token with "Zone:DNS:Edit" permission
   - Limit to specific zone (your domain)

2. **Add to Temps:**
   ```
   Settings > DNS Providers > Add Cloudflare
   - API Token: [your-token]
   - Zone ID: [from Cloudflare domain overview]
   ```

3. **Add Wildcard Domain:**
   ```
   *.example.com
   ```

## DNS Records Reference

### Subdomain (Recommended)

```
app.example.com -> CNAME -> your-project.temps.io
```

### Apex Domain

```
example.com -> A -> [Temps IP]
```

Or with Cloudflare (CNAME flattening):
```
example.com -> CNAME -> your-project.temps.io (proxied)
```

### Wildcard

```
*.example.com -> CNAME -> your-project.temps.io
```

## Certificate Management

### Automatic Renewal

Temps automatically renews certificates 30 days before expiration.

### Certificate Status

Check certificate status in Dashboard > Domains:
- **Pending**: DNS verification in progress
- **Active**: Certificate issued and active
- **Expiring**: Renewal scheduled
- **Failed**: Check DNS configuration

### Force Renewal

If needed, manually trigger renewal:
1. Go to Domains
2. Click domain
3. Click "Renew Certificate"

## Multiple Domains

Add multiple domains to the same project:

```
app.example.com     -> Production
staging.example.com -> Staging
api.example.com     -> API routes
```

Each domain gets its own certificate.

## Redirects

### www to non-www

Add both domains and configure redirect:

1. Add `example.com` (primary)
2. Add `www.example.com` (redirect)
3. Set `www.example.com` to redirect to `example.com`

### HTTP to HTTPS

Automatic - all HTTP requests redirect to HTTPS.

## Troubleshooting

**DNS not propagating?**
- Wait up to 48 hours for propagation
- Check with: `dig app.example.com`
- Use DNS checker: dnschecker.org

**Certificate stuck pending?**
- Verify CNAME/A record is correct
- Check no conflicting records exist
- Ensure domain isn't proxied (orange cloud in Cloudflare) during verification

**Wildcard not working?**
- Requires DNS provider integration
- Verify Cloudflare API token has correct permissions
- Check Zone ID is correct

**SSL certificate errors?**
- Clear browser cache
- Check certificate in browser: `https://app.example.com`
- Verify intermediate certificates are served

## API Reference

### Add Domain

```bash
curl -X POST https://your-temps.com/api/projects/123/domains \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"domain": "app.example.com"}'
```

### List Domains

```bash
curl https://your-temps.com/api/projects/123/domains \
  -H "Authorization: Bearer $TOKEN"
```

### Delete Domain

```bash
curl -X DELETE https://your-temps.com/api/projects/123/domains/456 \
  -H "Authorization: Bearer $TOKEN"
```

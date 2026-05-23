#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

echo "Starting Keycloak for Temps local SSO testing..."
docker compose up -d

echo "Waiting for Keycloak on http://localhost:8180 ..."
for i in $(seq 1 60); do
  if curl -sf "http://localhost:8180/realms/temps/.well-known/openid-configuration" >/dev/null 2>&1; then
    echo "Keycloak ready."
    break
  fi
  if [ "$i" -eq 60 ]; then
    echo "Keycloak did not become ready in time. Check: docker logs temps-keycloak"
    exit 1
  fi
  sleep 2
done

cat <<'EOF'

Keycloak is running for temps-ee OIDC testing.

Admin console
  URL:      http://localhost:8180/admin
  User:     admin
  Password: admin

Temps realm
  Issuer:   http://localhost:8180/realms/temps

OIDC client (pre-configured)
  Client ID:     temps-ee
  Client secret: temps-ee-dev-secret
  Redirect URIs: http://localhost:9081/api/auth/oidc/callback
                 http://localhost:9100/api/auth/oidc/callback  (EE web dev)

Test users
  sso-admin / sso-admin  (group: temps-admins, role: admin)
  sso-user  / sso-user   (group: temps-users, role: user)

Configure Temps EE (http://localhost:9081)
  Settings → Authentication → Add SSO Provider
    Template:      Keycloak
    Name:          Keycloak — Local
    Issuer URL:    http://localhost:8180/realms/temps
    Client ID:     temps-ee
    Client Secret: temps-ee-dev-secret
    Redirect URL:  copy from UI (must match /api/auth/oidc/callback on your origin)
    Scopes:        openid profile email roles
    Group claim:   groups
    Role claim:    roles
    Default role:  user
    Enabled:       off until Test connection passes
    Auto-provision: on

Suggested role mapping (after save)
  temps-admins → admin
  temps-users  → user

Test login
  1. Enable provider after Test connection succeeds
  2. Log out of Temps EE
  3. Use SSO on the login page

Stop Keycloak
  docker compose -f temps/tools/keycloak-dev/docker-compose.yml down

EOF

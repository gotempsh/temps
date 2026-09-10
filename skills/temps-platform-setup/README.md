# Temps Platform Setup

Provision a self-hosted Temps instance on an explicitly authorized machine,
verify it, and connect it to a named local CLI context through browser device
authorization.

## End-to-end outcome

The skill guides an agent to:

1. confirm the exact host and non-secret installation choices;
2. authenticate and review `https://temps.sh/deploy.sh` and its executable
   dependency chain before running anything as root;
3. run a bounded QuickStart, advanced, or local installation;
4. expose only allowlisted non-secret install results;
5. verify the console URL and service health;
6. start the pinned Temps CLI login itself;
7. give the user the short-lived browser approval URL and code;
8. keep polling until approval, then verify the named context with `whoami`.

Raw installer output and `setup-result.json` can contain an administrator
password and API key. They remain on the target server and are never copied
into chat. CLI authentication uses browser approval, not a pasted token.

See [SKILL.md](SKILL.md) for the complete provisioning, authorization, safety,
and platform-configuration workflow.

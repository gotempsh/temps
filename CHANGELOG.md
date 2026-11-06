# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-beta.61] - 2026-11-06

### Added

- **nodes:** Join a Mac as a worker with its Docker app, as the user and without sudo ([#1824](https://github.com/gotempsh/temps/issues/1824))

### CI

- **release:** Build the macOS binaries on macos-15 ([#1830](https://github.com/gotempsh/temps/issues/1830))

### Documentation

- **adr:** Record how a Mac joins as a worker with no VM to manage (ADR 090)
- **adr:** State that Mac workloads can reach the node API through the Mac (ADR 090)
- **adr:** Say the Mac policy window lasts until a successful pass, not 15 s (ADR 090)

### Fixed

- **web:** Stop the dark theme rendering light-only colours
- **web:** Keep cache-hit badges readable and only accept dark colours in the scan
- **web:** Match native controls to the console theme and keep the Cloudflare link readable
- **web:** Unbreak the console build and reject variant-only dark colours in the scan
- **cli:** Restart the agent service after temps join rewrites its config ([#1829](https://github.com/gotempsh/temps/issues/1829))

### Testing

- **web:** Keep the colour-scheme test beside globals.css
- **web:** Require an all-sides dark border and a real arbitrary colour in the scan

## [0.1.0-beta.60] - 2026-11-05

### CI

- **release:** Link the Linux ARM64 binary with lld so it stops overflowing call range

### Fixed

- **git:** Block connections whose refresh token is rejected and stop losing rotated tokens
- **web:** Explain a rejected GitLab authorization beside Reauthorize
- **git:** Keep a stale refresh from overwriting a new token and refresh tokens of unknown lifetime
- **web:** Stop sending GitLab App connections to a Reauthorize action they lack
- **web:** Show GitLab App connections their recovery in the connections list
- **web:** Stop showing a connection with a rejected token as healthy
- **web:** Explain the machine and container address ranges on the Worker Nodes page ([#1821](https://github.com/gotempsh/temps/issues/1821))
- **web:** Simplify build configuration, status pages, analytics, email and projects screens ([#1816](https://github.com/gotempsh/temps/issues/1816))
- **web:** Give new workers a join command that installs the plane's version with sudo ([#1819](https://github.com/gotempsh/temps/issues/1819))
- **web:** Check tunnel workers are online before creating anything, and show hostnames as a pattern ([#1820](https://github.com/gotempsh/temps/issues/1820))
- **migrations:** Drop telemetry foreign keys after boot, not in one migration transaction
- **test-utils:** Return the typed schema error from the post-migration helper
- **schema:** Harden the telemetry foreign key detach against review findings

### Testing

- **release:** Fail the workflow contract when the ARM64 lld check is weakened

## [0.1.0-beta.59] - 2026-11-05

### Added

- **nodes:** Encrypt values sent to workers with the node's public key
- **git:** Reauthorize existing OAuth connections
- **docker-util:** Add a crate for capped exec capture, image pull, ensure-network and tar upload

### Build

- **deps:** Bump bollard to 0.21.1 and testcontainers to 0.28
- **status-page:** Remove the unused futures dependency
- **deps:** Drop anyhow from crates that no longer use it

### CI

- **rust-tests:** Report what fills the disk in unit test jobs
- **rust-tests:** Keep the disk report going after a failed command
- **release:** Stop the Linux ARM64 build from losing its runner
- **release:** Log ARM64 runner resources before the first sleep
- **release:** Check every ARM64 resource probe runs before the first sleep
- **rust-tests:** Run the temps-ai-agents unit tests
- **rust-tests:** Stop a cache restore before it fills the disk
- **rust-tests:** Keep the disk guard honest through the whole restore
- **rust-tests:** Keep stopping a restore while the disk guard scans
- **otel:** Run the Cloud-primary write-mode and outbox load tests
- **tests:** Add a tool that compares the CI test filters with the workspace
- **tests:** Run every test binary in CI and fail when one is left out
- **rust-tests:** Keep the moved protocol and actor token tests running
- **tests:** Report a missing cargo as a coverage tool error
- **tests:** Exclude the rotated-out cursor test by name instead of ignoring it
- **tests:** Correct the allow-list reason for the mesh lockdown tests
- **tests:** Run the starters discovery tests in unit-integration
- **tests:** Run Rust Tests on pull requests into the refactor branches
- **tests:** Run the temps-docker-util tests in the Docker group
- **nextest:** Run the tests sharing the sandbox image one at a time
- **tests:** Run Rust Tests on pull requests into main only
- **e2e:** Deploy fork pull requests from the fork

### Documentation

- **deployments:** Qualify secret mounts and list every TLS-only sandbox channel
- **nodes:** Say which way each TLS-only response travels
- **skills:** Adding a migration is adding a file
- **crates:** Regenerate the crate map and lower the rule-drift baseline
- **alarms:** Add temps-alarms to the crate map
- **alarms:** Point release testing at the moved rule catalog tests
- **deployments:** Fix the get_exposed_ports example
- **core,plugin-sdk,query-mongodb:** Fix doc examples that no longer compile
- **crates:** Regenerate the crate map for temps-plugin-protocol
- **ci:** State the coverage guard's measured runtime
- **crates:** Regenerate the crate map after merging the integration branch
- **deployments:** Correct the comment on unsupported job types
- **deployments:** Document what each job reads from its row
- **config:** Point the environment hostname template at its default's new home
- **deployments:** Add crate rules for the deployment pipeline
- **nodes:** Add crate rules for node lifecycle and placement
- **providers:** Add crate rules for the service manager and the engine contract
- **proxy:** Add crate rules for the request path
- **backup:** Add crate rules for backups and restores
- **ai:** Add crate rules for chats and unattended agent runs
- **core:** Add crate rules for plugins and shared contracts
- **migrations:** Add crate rules for migrations and entities
- **core:** Correct the documented middleware priority order

### Fixed

- **deployments:** Keep secrets out of final image metadata on custom builds
- **deployer:** Treat a truncated exec stream as a failed probe on bollard 0.21
- **deployments:** Keep the running Compose stack when a deploy fails before replacing it
- Resolve the 20 open automated-qa issues across API, workers, proxy, AI gateway and backup ([#1779](https://github.com/gotempsh/temps/issues/1779))
- Image transfer timeouts, Dragonfly startup and false deployment reverts ([#1787](https://github.com/gotempsh/temps/issues/1787))
- **web:** Report refused console actions accurately and with the reason ([#1794](https://github.com/gotempsh/temps/issues/1794))
- **git:** Preserve reauthorized grants during concurrent refreshes
- **api:** Regenerate OAuth reauthorization client routes
- **git:** Keep token refresh independent of sync status
- **deployer:** Stop following build status when a secret-mount build is cancelled
- **deployments:** Pass public values when a build falls back to the control plane
- **deployer:** End the BuildKit session with every secret-mount build
- **deployments:** Serve a secret mount however BuildKit names it
- **deployments:** Withhold secrets from ARGs hidden by Dockerfile line syntax
- **cli:** Make rollback -y skip the confirmation prompt
- **deployments:** Run the default health check on rollback and promotion
- **cli:** Fail a targetless rollback without a terminal before fetching history
- **ai-agents:** Install the microsandbox runtime the SDK requires
- **entities:** Keep health-check timeout bounds with the config type
- **deps:** Update pulldown-cmark to 0.13.6 for RUSTSEC-2026-0433
- **otel:** Update projects when disconnecting from Cloud or falling back on quota
- **deployer:** Let the release binary compile again
- **ai-agents:** Install the microsandbox runtime the SDK accepts
- **core:** Redact the master key from EncryptionService Debug output
- **core:** Keep key characters out of cookie and ECIES key parse errors
- **providers:** Answer engine validation, not-found and conflict errors with their own status
- **docker-util:** Restore feature-gated test import and drop a needless borrow
- **providers:** Return the daemon's own error when the Temps network cannot be created
- **ai-chat:** Discard a stale harness runtime before the salvage reply
- **ai-chat:** Give each attachment conflict its own explicit error title
- **git:** Name every provider failure behind a repository creation error
- **monitoring:** Decide the message of every cron monitor error explicitly
- **funnels:** Document the funnel update and delete responses in the API
- **providers:** Document the 400 that start, stop, env var and credential endpoints can return
- **providers:** Answer a start on a missing imported container with a 404
- **sandbox:** Stop a kill by pattern from restarting the whole sandbox
- **backup:** Keep a cancelled backup failed when its engine finishes late
- **backup:** Stop one bad cron expression from ending all scheduled backups
- **backup:** Start only one run when a schedule is triggered concurrently
- **backup:** Fail the backup when its engine panics instead of leaving it running
- **backup:** Fail child rows when a backup is cancelled or orphaned by a restart
- **backup:** Keep handling backup requests and cancels while every slot is busy
- **deployments:** Mark jobs interrupted by a restart as failed, not successful
- **deployments:** Withhold failure report logs when secrets cannot be decrypted
- **deployments:** Remove deployment containers even when log storage is broken
- **deployments:** Point the missing Docker socket error at ADR 079
- **ai-agents:** Leave a preview gateway owned by another instance running on startup
- **ai-agents:** Make cancelling an agent run stop its sandbox and stick
- **cli:** Exit 0 from temps projects delete after deleting the default project
- **docker-housekeeping:** Keep the Docker socket address out of disk usage errors
- **schema:** Stop temps migrate --dry-run creating the migration ledger
- **entities:** Keep stored secrets out of entity JSON and debug output
- **nodes:** Log why a worker's remote deployer could not be created
- **cli:** Serve the node state and instance env routes on the mesh listener
- **geo:** Keep temps serve running when the GeoLite2 database is missing
- **cli:** Accept 64-bit instance ids on the node mesh env route
- **providers:** Refuse to upgrade an imported service instead of creating a second container
- **providers:** Derive the data explorer's database password with HMAC-SHA256
- **providers:** Delete an imported service without touching the operator's container
- **providers:** Answer cluster routes by error type, not by words in the message
- **providers:** Stop leaving anonymous volumes behind after restores and upgrades
- **providers:** Stop repeating the input in the invalid service type error
- **git:** Validate the GitHub App manifest response and never match an empty webhook secret
- **git:** Report manifest setup failures with the shared error builder
- **git:** Require a name in the GitHub App manifest reply
- **console:** Hold the random console ports until they are served
- **serve:** Size the server's database pool from TEMPS_DB_* again
- **deployments:** Stop and start paused deployments on the node that runs them
- **deployments:** Keep pause, resume and deploy from racing over a paused deployment
- **deployments:** Give a restarted replica the full failure threshold before alarming
- **serve:** Stop startup hanging on geo downloads and dead pool connections ([#1812](https://github.com/gotempsh/temps/issues/1812))
- **deployments:** Remove a resumed deployment's containers when a newer deploy replaces it
- **deployments:** Keep a deploy's teardown from removing a deployment being paused
- **deployments:** Limit how long an unreachable worker can block pause and resume
- **geo:** Resolve cached addresses again when a late database arrives ([#1815](https://github.com/gotempsh/temps/issues/1815))

### Miscellaneous

- **migrations:** Lower the rule-drift baseline for the shorter migration module
- **refactor:** Regenerate the crate map and lock in the lower rule-drift count
- **core:** Lower the anyhow_in_library rule baseline
- **tools:** Remove the unreferenced Keycloak dev harness
- **tools:** Remove the unreferenced backup stress benchmark
- Remove the unreferenced acceptance-tests runner
- **api:** Generate @temps-sdk/api from the canonical spec and drop its copy
- **api:** Regenerate @temps-sdk/api from the canonical spec
- **docker-util:** Format the node agent pull and regenerate the crate map
- **rules:** Lower the temps-deployments rule-drift baseline
- **deployments:** Lower the long-function baseline after the replica phase split
- **rule-drift:** Lower the baseline after the catch-all and funnel fixes
- **nodes:** Record the temps-nodes crate in the docs and rule baseline
- **rule-drift:** Lower the baseline after the typed notification errors
- **rule-drift:** Lower the baseline after the typed git errors
- **rule-drift:** Lower the baseline after naming error fields
- **api:** Regenerate the OpenAPI spec and both clients
- **openapi:** Refresh the spec and generated clients

### Refactor

- **migrations:** Generate the migration list from the migration files
- **backup:** Merge temps-backup-core into temps-backup as `execution`
- **embeddings:** Keep only the hash tokenizer error tracking uses
- **error-tracking:** Merge temps-embeddings into temps-error-tracking as `tokenizer`
- **cli:** Merge temps-static-files into temps-cli
- **authz:** Split the authorization primitives into temps-authz
- **ai,analytics,audit:** Depend on temps-authz instead of temps-auth
- **blob,dns,email,flags,geo,import,infra:** Depend on temps-authz
- **kv,logs,mcp,monitoring,netpol,notifications:** Depend on temps-authz
- **otel,proxy,webhooks,error-tracking:** Depend on temps-authz
- **core:** Share one constant-time comparison for secrets and token hashes
- **import:** Share the importers' sanitize_slug through temps-import-types
- **clickhouse:** Build the real-ClickHouse test clients with temps-clickhouse
- **core:** Run status-page and log-aggregator retries through RetryConfig
- **environments:** Give temps-projects' env var service the shared EnvVarError
- **core:** Hash stored tokens with one shared sha256_hex
- **core:** Share HTML and LIKE-pattern escaping through temps-core
- **session-replay,project-groups:** Depend on temps-authz
- **config:** Move settings handlers and ConfigPlugin into temps-config-handlers
- **entities:** Delete the unused tokenizer copy
- **presets:** Move the Compose security catalog out of temps-entities
- **presets:** Move stored preset behaviour out of temps-entities
- **config:** Move deployment config logic out of temps-entities
- **alarms:** Move the alarm service into a new temps-alarms crate
- **alarms:** Move default alert rule seeding into temps-alarms
- **alarms:** Depend on temps-alarms from providers, git, backup and otel
- **test-utils:** Move TestDatabase into a dev-only temps-test-utils crate
- **schema:** Move the migrator out of temps-database into temps-schema
- **core:** Stop re-exporting temps-memory from temps-core
- **deployments:** Move the workflow engine out of temps-core
- **core:** Move the external plugin protocol to temps-plugin-protocol
- **core:** Move project templates to temps-presets
- **core:** Move AppSettings and public ingress to temps-config
- **core:** Return a typed EncryptionError from EncryptionService
- **core:** Return a typed AuditError from the audit traits
- **core:** Remove ServiceError::Internal
- **providers:** Move the MariaDB data browser into temps-query-mariadb
- **config:** Move the control plane's node agent TLS clients into temps-config
- **ai-agents:** Move AgentError and the shared sandbox constants into temps-agent-core
- **backup:** Use temps-docker-util for WAL-G and binlog execs and image pulls
- **providers:** Use temps-docker-util for exec capture, pulls and uploads
- **ai-agents:** Use temps-docker-util for sandbox step execs and image pulls
- **docker:** Use temps-docker-util for container exec and plain image pulls
- **core:** Move ensure_bridge_network to temps-docker-util
- **ai-cli:** Make the AI CLI providers a standalone temps-ai-cli crate
- **providers:** Move the engine contract into temps-providers-core
- **sandbox:** Make the sandbox backends a standalone temps-sandbox-providers crate
- **ai-chat:** Move the tool loop into its own module
- **ai-chat:** Give the tool loop an explicit budget
- **ai-chat:** Build the tool loop's executors outside the loop
- **ai-chat:** Hold the tool loop's progress in state structs
- **ai-chat:** Extract how a chat turn finishes
- **ai-chat:** Extract the tool loop's salvage call
- **ai-chat:** Extract tool dispatch from the tool loop
- **ai-chat:** Extract one provider round from the tool loop
- **ai-chat:** Extract a chat turn's claim check and monitor
- **ai-chat:** Extract how a chat turn is assembled
- **deployments:** Add a job registry and move the configure jobs
- **deployments:** Move the mark, screenshot and scan jobs to the registry
- **deployments:** Move the capture and persist jobs to the registry
- **deployments:** Move the pull, verify and source bundle jobs to the registry
- **deployments:** Move the static site jobs to the registry
- **deployments:** Move the repository download job to the registry
- **deployments:** Move the compose job to the registry
- **deployments:** Move the image build job to the registry
- **deployments:** Move the image deploy job to the registry
- **deployments:** Make the job registry the only dispatcher
- **proxy:** Move request_filter's body behind a stage result
- **proxy:** Take reading and stripping the request out of request_filter
- **proxy:** Take the status-page hostname stage out of request_filter
- **proxy:** Take the sandbox preview stage out of request_filter
- **proxy:** Make the signed-in gate a request_filter stage
- **proxy:** Take the host wake stage out of request_filter
- **proxy:** Take matching the path route out of request_filter
- **proxy:** Take the path target wake stage out of request_filter
- **proxy:** Take the unavailable path target stage out of request_filter
- **proxy:** Take a project's checks out of request_filter
- **proxy:** Take the connection cap stage out of a project's checks
- **proxy:** Take the request policy stage out of a project's checks
- **proxy:** Take the access decision's responses out of a project's checks
- **proxy:** Take the Shield stage out of a project's checks
- **proxy:** Take the password wall stage out of a project's checks
- **proxy:** Take the solver files stage out of request_filter
- **proxy:** Make the ACME challenge one request_filter stage
- **proxy:** Take the on-demand TLS stage out of request_filter
- **proxy:** Take the HTTPS redirect stage out of request_filter
- **proxy:** Take the domain redirect stage out of request_filter
- **proxy:** Take preparing the upstream request out of request_filter
- **proxy:** Take the second domain redirect check out of request_filter
- **proxy:** Take the static files stage out of request_filter
- **proxy:** Take the persisted asset stage out of request_filter
- **proxy:** Take the stale chunk stage out of request_filter
- **proxy:** Take the admin gate stage out of request_filter
- **proxy:** Answer every wake refusal from one place
- **proxy:** Split the sign-in and password verify answers out of their stages
- **proxy:** Lock in the shorter request filter in the rule baseline
- **cli:** Register plugins from tables that read in startup order
- **providers:** Move the Redis engine into temps-providers-redis
- **node-agent:** Call database engines and the plugin compiler through injected interfaces
- **node-agent:** Pass the injected engines in the agent server options
- **deployments:** Split replica deployment into named phases with a test per failure path
- **providers:** Move the MongoDB engine into temps-providers-mongodb
- **deployments:** Load deployment projects and environments through the remote deployment service
- **deployments:** Serve the edge route table and node lookups from services
- **providers:** Move the MariaDB engine into temps-providers-mariadb
- **providers:** Move the object storage engines into temps-providers-s3
- **providers:** Move the PostgreSQL engines into temps-providers-postgres
- **nodes:** Move worker node code into temps-nodes
- **notifications:** Report provider failures with typed errors that hide webhook secrets
- **traefik-discovery:** Move Traefik route discovery into its own crate
- **providers:** Fold the data browser drivers into their engines
- **ci-tokens:** Move deployment tokens and CI dispatch into their own crate
- **git:** Tell missing files, refused tokens and rate limits apart when reading repositories
- **git:** Report local git failures with typed errors that drop URL credentials
- **docker-housekeeping:** Move Docker cleanup and disk usage into their own crate
- **monitoring:** Name the monitor, operation and path in cron, disk and outage errors
- **ai-chat:** Give identity error variants named fields, keeping status and detail
- **git:** Give public repository error variants named fields, keeping status and detail
- **deployments:** Remove the unused environment-wide container operations

### Styling

- **migrations:** Apply rustfmt to the generated migration list
- **providers:** Format the test-support attributes
- **ai-chat:** Format the tool loop tests
- **deployments:** Format the rollback tests after the helper path change

### Testing

- **deployments:** Skip the rollback probe tests when Docker is unavailable
- **ai-agents:** Make the Docker sandbox tests hold up under nextest
- **ai-agents:** Stop the kill test matching its own helper shell
- **migrations:** Pin shipped migration order in a checked-in fixture
- **backup:** Cover the cancel helpers, job processor and orphan reconcile
- **authz:** Cover the role mapping, guard 403 shape and RequireAuth
- **config-handlers:** Cover the settings router's 401 and the plugin's OpenAPI document
- **alarms:** Cover moved entry points that had no direct test
- **alarms:** Cover seed_default_rules capacity and insert paths on a mock
- **test-utils:** Share the identical docker_available helper
- **test-utils:** Share the error-tracking setup_test_db helper
- **otel:** Stop Cloud-primary tests depending on two clocks agreeing
- **monitoring:** Add the column the HTTP check tests' env_vars table was missing
- **cloud:** Align console-access tests with email and ingress-grant changes
- **clickhouse:** Drop a premise the clickhouse crate no longer meets
- **providers,ai-agents:** Update golden container fixtures to what the engines send
- **sandbox:** List the egress-policy route in the Vercel surface guard
- **core:** Pin the external plugin wire format byte for byte
- **sandbox:** List every route the Vercel surface guard was missing
- **vm-agent:** Mark the rotated-out cursor test as known broken on ext4
- **cli:** Stop the OTLP leak test failing on a random span id
- Pin the client-visible body of fail-closed audit paths
- **dns,providers,migrations:** Run the database suites CI was skipping
- **ci:** Check which pull requests run Rust Tests and how tests guard on the database URL
- **docker:** Remove a test container that would not start
- **ai-chat:** Pin what a chat turn shows and persists for each way it ends
- **ai-chat:** Use expect_err in the tool loop tests
- **deployments:** Pin what every job type string builds
- **proxy:** Add a benchmark of the request path
- **proxy:** Stop the request path benchmark waiting for a first route load
- **proxy:** Pin the order of the checks in request_filter
- **proxy:** Pin the status page host and preview ahead of the signed-in gate
- **proxy:** Keep the request path bench out of the test coverage check
- **proxy:** Stop changing the environment in the request path fixtures
- **ai-chat:** Read the Redis engine from its own crate
- **e2e:** Wait for the outage incident in the monitoring scenario
- **providers:** Assert the typed error when MariaDB refuses an upgrade
- **cli:** Fail if a serve profile's local-workload setting stops reaching environments
- **authz:** Record temps-nodes and temps-ci-tokens in the guard snapshots
- **migrations:** Name the latest shipped migration in the order failure message

## [0.1.0-beta.58] - 2026-11-03

### Added

- **web:** Rework the v2 project layout for findability and consistency
- **web:** Title project pages one level below the project name
- **web:** Show deployment health, reachability and the repository in the project bar
- **web:** Open a segment's visitors from its breakdown row
- **web:** Lead every project and environment tab with an icon
- **web:** Slim the project header to name, environment and status
- **web:** Rework the Build & deploy settings into consistent groups
- **web:** One save row, titles and sentence case across settings pages
- **web:** Open a Shield event's client address on its IP page
- **web:** Open every client address on its IP page
- **web:** Use the shared page header on the remaining hand-built titles
- **web:** Sentence case and one Save label across the console
- **web:** Give every tab strip icons
- **web:** Split cron jobs and check-ins into their own pages
- **web:** Edit an environment's variables as .env text
- **environments:** Suggest storing credential-looking variables as secrets
- **entities:** Add an atomic per-project deployment counter
- **web:** Retire the AI workspace page and tighten console lists ([#1734](https://github.com/gotempsh/temps/issues/1734)) [**BREAKING**]

### CI

- **sandbox:** Read the managed daemon image pin from application_workspace.rs
- Lower the rule-drift baseline after the sandbox_service split
- Lower the rule-drift baseline after the Docker sandbox provider split
- Lower the temps-cli rule-drift baseline after the console startup split
- Lower the rule-drift baseline after splitting the sandbox create, snapshot and Dockerfile functions
- Lower the rule-drift baseline after the route table split

### Documentation

- **design:** Section tabs carry icons, counts use the badge
- **refactor:** Record crates refactor progress and add its test plan
- **ai-chat:** Describe the handlers module layout
- Describe embedding binaries and downstream consumers without naming them

### Fixed

- **web:** Address review findings on the project layout
- **web:** Environment picker for monitoring-only projects
- **web:** Show loading and failure in the monitoring header's environment picker
- **web:** Restore the project's identity in the project bar
- **web:** Align form pages with the header instead of centering them
- **ingress:** Say why a control-plane-only environment has no public address
- **web:** Drop tab icons on phones so strips keep their tabs in view
- **web:** Sentence case and "Set up" on the remaining setup prompts
- **web:** Offer no Visit for an environment that only runs a worker
- **web:** Drop project tab icons on phones, like the shared tabs
- **web:** One Save row for a service's security settings
- **web:** Show IP access addresses only for an allow or block list
- **web:** One empty state for a cron page with nothing on it
- **web:** Scroll a section's current tab into view on a phone
- **web:** Give Uptime, Cron jobs and Check-ins their tab icons in the palette
- **agents:** Warn once an hour about a preview gateway failure that keeps repeating
- **environments:** Require secrets:read or environments:write to reveal values
- **web:** Convert a suggested secret without revealing its value
- **web:** Keep the environment variables table stable and compact
- **environments:** Apply the caller's project role when revealing values
- **web:** Explain a refused reveal in the .env editor and stop at the first one
- **web:** Fewer controls on the environment variables page
- **web:** Make the variable filters an icon toggle
- **web:** Show variable checks as status icons
- **web:** Keep deployment step headers and URL bar tidy in dark mode
- **web:** Stop the live globe blanking the whole console on first visit
- **web:** Switch application logs between live and history in the toolbar
- **web:** Slim the Add Variable button on the variables page
- **web:** Confirm before the .env editor removes variables
- **web:** Name the variable Filters toggle by its content
- **environments:** Lock the variable while dismissing its secret suggestion
- **web:** Keep values when a .env edit partly fails or the list refreshes
- **web:** Wrap the live log filters and show full step names on hover
- **console:** Let the assistant dismiss a secret suggestion with approval
- **status-page:** Show the page title next to the logo in the public header
- **status-page:** Keep the status page title visible on narrow screens
- **deployments:** Give concurrent deployments of a project distinct slugs
- **deployer:** Refuse to replace a container that belongs to another deployment
- **deployer:** Box the container-name conflict so DeployerError stays small
- **compose:** Pin ClickHouse 26.8.36.3 for CVE-2026-116366
- **cli:** Add --header to notifications add/update so webhook URLs can change
- **cli:** Keep --header values out of error messages and keep "__proto__" headers
- **cli:** Reject control characters in --header values and never echo header names
- **cli:** Validate --header values before trimming
- **proxy:** Stop dropping Markdown requests whose response is passed through
- **cli:** Log every temps crate at the default level, plugin crates included
- **web:** Stop the .env editor widening a variable to every environment
- **web:** Use the standard responsive label and colour tokens
- **environments:** Keep a dismissed secret suggestion when the value is unchanged
- **api:** Document the OTel S3 archive credential inputs
- **deps:** Bump zstd-safe to 7.4.0 and zstd to 0.13.4 to resolve RUSTSEC advisories ([#1771](https://github.com/gotempsh/temps/issues/1771))
- **providers:** Validate the probe host before retargeting it on workers ([#1776](https://github.com/gotempsh/temps/issues/1776))

### Miscellaneous

- **api:** Regenerate clients for the secret-suggestion endpoint
- **rule-drift:** Lower the temps-ai-chat file-size baseline
- **deps-dev:** Bump eslint in /web in the web-minor-patch group ([#1747](https://github.com/gotempsh/temps/issues/1747))

### Refactor

- **web:** Make the grouped project navigation the only one
- **web:** One rate limiting group on the Security and Rate limiting pages
- **environments:** Propagate audit serialization errors with ?
- **ai-chat:** Move handlers.rs to handlers/mod.rs
- **ai-chat:** Move the handler tests into sibling files
- **ai-chat:** Move handler DTOs, error mapping and limits into modules
- **ai-chat:** Move the shared handler access checks into handlers/access.rs
- **ai-chat:** Move the application handlers into modules
- **ai-chat:** Move the workspace handlers into modules
- **ai-chat:** Move the conversation, message and attachment handlers into modules
- **ai-chat:** Move the permission and pending-action handlers into modules
- **ai-chat:** Move chat readiness and the sandbox MCP transport into modules
- **ai-chat:** Move service.rs to service/mod.rs
- **ai-chat:** Move the conversation service tests into service/tests.rs
- **ai-chat:** Move the live wire events into service/wire_events.rs
- **ai-chat:** Move conversation title generation into service/titles.rs
- **ai-chat:** Move turn persistence helpers into service/persistence.rs
- **ai-chat:** Move the managed process tools into service/managed_process.rs
- **ai-chat:** Move the turn budget into service/prompt_budget.rs
- **ai-chat:** Move tool-call dispatch into service/tool_dispatch.rs
- **sandbox:** Move sandbox_service.rs to sandbox_service/mod.rs
- **sandbox:** Split sandbox_service into modules by responsibility
- **ai-agents:** Move sandbox/docker.rs to sandbox/docker/mod.rs
- **ai-agents:** Split the Docker sandbox provider into modules by responsibility
- **ai-api-tools:** Move the assistant's API allowlists out of the console startup file
- **cli:** Split the console startup function into ordered steps
- **ai-agents:** Build the sandbox Dockerfile from named sections per concern
- **ai-agents:** Split sandbox container creation into named steps with one rollback point
- **ai-agents:** Split taking a Docker snapshot into inspect, commit, verify and artifact steps
- **routes:** Move route_table.rs to route_table/mod.rs
- **routes:** Split load_routes into its seven loading phases
- **proxy:** Move proxy.rs to proxy/mod.rs
- **proxy:** Split self-contained groups out of proxy/mod.rs
- **proxy:** Wire the split proxy modules with the narrowest visibility
- **proxy:** Keep the Shield doc comment on enforce_shield

### Revert

- **refactor:** Keep the refactor plan and test plan out of the repository

### Testing

- **web:** Update project navigation E2E for the single layout
- **web:** Point e2e specs at the renamed labels
- **web:** Match four more renamed labels in the console e2e specs
- **web:** Expect both Add domain buttons on an empty Domains page
- **web:** Expect the Check-ins tab in the project navigation spec
- **web:** Expect the Edge rules tab in the project navigation spec
- **node-agent:** Map a container-name conflict with another deployment to 409
- **ai-chat:** Cover authentication and authorization of every chat route
- **ai-chat:** Exercise each conversation service helper module directly
- **sandbox:** Cover the lifecycle, preview, placement, reconcile, links and data network entry points
- **ai-agents:** Cover the Docker exec, create and snapshot entry points without the sandbox image
- **cli:** Pin the console startup order, init failure and router assembly
- **ai-agents:** Pin the generated sandbox Dockerfile of every runtime preset with golden fixtures
- **routes:** Exercise every route-loading phase against a real database
- **proxy:** Cover the entry points of the split proxy modules
- **deployments:** Check stop, start and restart refuse a microVM replica
- **edge:** Check cache eviction deletes the evicted file from disk
- **external-plugins:** Check events for a full or closed channel are refused
- **plugin-sdk:** Check events past the buffer are dropped while requests go through
- **git:** Check archive progress past the buffer never blocks the download
- **domains:** Check the LETSENCRYPT_MODE shim stores the setting and its audit row

## [0.1.0-nightly.20261101.243d238c1] - 2026-11-01

### Added

- **workload:** Resolve an image's named USER for microVM replicas ([#1677](https://github.com/gotempsh/temps/issues/1677))
- **edge-rules:** Project edge rules to block, allow, challenge, redirect or log requests ([#1693](https://github.com/gotempsh/temps/issues/1693))
- **config:** Move runtime env configuration into stored settings

### Build

- Pin the Rust toolchain to 1.97.1 across local builds and CI
- Run full-workspace clippy at pre-push, changed crates at commit
- **lints:** Deny unwrap/expect/dbg/todo workspace-wide and ban context, env and unbounded channels
- Drop never-enabled cargo features
- Remove unused dependencies
- Inherit member dependency versions from the workspace

### CI

- Add CLAUDE.md rule-drift counter with per-crate baseline
- Add cargo-deny licence/source/ban gate and weekly coverage report
- Record the rule-drift baseline
- Fail pull requests that add unused dependencies
- Lower the rule-drift baseline after the cargo-hygiene deletions
- **deny:** Give each licence clarification the license-files field cargo-deny 0.20 requires
- Pin the toolchain to 1.99.1, the stable release the build caches use
- Lower the rule-drift baseline after both temps-deployer splits
- Lower the rule-drift baseline after both temps-providers splits
- Lower the rule-drift baseline after the temps-projects split
- Lower the rule-drift baseline after the git_provider_manager split
- Lower the rule-drift baseline after the temps-backup split
- Pin only the Rust version, not components, in rust-toolchain.toml
- Lower the rule-drift baseline after the deployment service split

### Documentation

- **plans:** Add crates refactor plan
- **plans:** Mark p0-stability lane merged
- **claude:** Allow sibling tests.rs for large files and reasoned #[ignore]
- Document the enforced lints, rule drift and public-text hygiene
- **plans:** Drop local build-environment detail from the execution notes
- **plans:** Mark p4-cargo-hygiene merged
- **plans:** Mark p1-guardrails merged and start the renames lane
- **plans:** Use the renamed crate names in pending refactor items
- **plans:** Mark the renames merged and start wave 2
- Generate a crate map and give every crate a description
- **adr:** Give every ADR a unique number and add a generated index
- **skills:** Add contributor skills for migrations, handlers, plugins and splits
- Make AGENTS.md the one rule file and slim what loads per session
- **plans:** Mark p6-docs merged
- **plans:** Mark the temps-deployer splits merged
- **plans:** Mark the providers and projects splits merged
- **plans:** Mark the temps-backup split merged
- **plans:** Mark the temps-git split merged
- **plans:** Mark the temps-deployments split merged

### Fixed

- **projects:** Delete projects without cascading through telemetry ([#1681](https://github.com/gotempsh/temps/issues/1681))
- **deps:** Patch security vulnerabilities reported by Dependabot
- **edge:** Lock-free route table, cert store and cache index
- **proxy:** Bound connection-filter refresh, ACME lookups and preview token reads
- **deployments,dns-resolver,metrics:** Non-poisoning locks and an evicting environment-lock map
- **backup,core:** Inject the job queue at construction, recover poisoned registry locks
- **ci:** Rerun temps-cli build script when dist is missing; avoid deprecated fetch_update
- **webhooks:** Drop the never-written webhook_deliveries.response_body
- **config:** Migrate each deprecated environment variable only once

### Miscellaneous

- Delete dead functions with no callers
- **agents:** Remove the unused iptables sandbox egress filter
- Delete unreferenced evidence screenshots
- Remove stale dead_code allows and the code they hid

### Refactor

- Replace production unwrap/expect and anyhow .context() with typed handling
- **channels:** Bound plugin, SDK and archive-progress channels
- **lints:** Bring the p0-stability modules under the workspace lints
- Take CachedPeerTable from temps-routes, not temps-proxy
- **deployer:** Drop the dependency on temps-dns
- **crates:** Rename temps-agent to temps-node-agent
- **crates:** Rename temps-agents to temps-ai-agents
- **crates:** Rename temps-telemetry to temps-product-telemetry
- **crates:** Rename temps-logs to temps-deployment-logs
- **crates:** Rename temps-dns-resolver to temps-node-dns
- **providers:** Move externalsvc/postgres.rs to postgres/mod.rs
- **providers:** Split externalsvc/postgres.rs into modules
- **deployer:** Move compose.rs to compose/mod.rs
- **deployer:** Split the compose module by responsibility
- **deployer:** Move docker.rs to docker/mod.rs
- **deployer:** Split the docker module along its impl seams
- **providers:** Move services.rs to services/mod.rs
- **providers:** Split services.rs into manager modules
- **projects:** Move project.rs to project/mod.rs
- **projects:** Move project service tests into project/tests.rs
- **projects:** Move preset resolution into project/presets.rs
- **projects:** Move root-directory validation into project/source_directory.rs
- **projects:** Move delivery provider selection into project/delivery.rs
- **projects:** Move naming and slug helpers into project/naming.rs
- **projects:** Move authorization guards into project/guards.rs
- **projects:** Move project reads into project/queries.rs
- **projects:** Move service-template code into project/templates.rs
- **projects:** Move provider webhook lifecycle into project/webhooks/
- **projects:** Move hosting and source toggles into project/hosting.rs
- **projects:** Move project deletion into project/deletion.rs
- **projects:** Move Git settings updates into project/git_settings.rs
- **projects:** Move settings updates into project/project_settings.rs
- **projects:** Move environment-variable passthroughs into project/variables.rs
- **projects:** Move pipeline triggers into project/pipeline.rs
- **projects:** Move project creation into project/create.rs
- **projects:** Move pure unit tests next to the code they test
- **git:** Move git_provider_manager.rs to git_provider_manager/mod.rs
- **git:** Split git_provider_manager into modules by responsibility
- **backup:** Move services/backup.rs to services/backup/mod.rs
- **backup:** Move shared backup test fixtures into test_support.rs
- **backup:** Move BackupError and its conversions into backup/error.rs
- **backup:** Move the service response types into backup/types.rs
- **backup:** Move access-scope resolution into backup/access_scope.rs
- **backup:** Move retention into backup/retention.rs
- **backup:** Move S3 source management into backup/s3_sources.rs
- **backup:** Move the S3 backup index into backup/index.rs
- **backup:** Move backup deletion into backup/deletion.rs
- **backup:** Move WAL-G backup deletion into backup/walg_deletion.rs
- **backup:** Move control-plane WAL-G backups into backup/walg.rs
- **backup:** Move control-plane backups into backup/postgres_backup.rs
- **backup:** Move control-plane restores into backup/restore.rs
- **backup:** Move read-only backup queries into backup/queries.rs
- **backup:** Move recovery-set publication into backup/recovery_set.rs
- **backup:** Move schedule management into backup/schedules.rs
- **backup:** Move schedule runs into backup/schedule_runs.rs
- **backup:** Move the backup scheduler loop into backup/scheduler.rs
- **backup:** Move external-service backups into backup/external_service.rs
- **deployments:** Move services.rs to services/mod.rs
- **deployments:** Move deployment service tests into services/tests.rs
- **deployments:** Move DeploymentError into services/error.rs
- **deployments:** Move URLs, links and custom domains into services/domains.rs
- **deployments:** Move deployment reads into services/queries.rs
- **deployments:** Move deployment settings and asset cache purge into services/settings.rs
- **deployments:** Move container operations into services/containers.rs
- **deployments:** Move owner container cleanup into services/cleanup.rs
- **deployments:** Move deploy guards into services/guards.rs
- **deployments:** Move deployment triggers into services/trigger.rs
- **deployments:** Move redeploys into services/redeploy.rs
- **deployments:** Move rollback and promotion reuse helpers into services/reuse.rs
- **deployments:** Move rollback into services/rollback.rs
- **deployments:** Move promotion into services/promote.rs
- **deployments:** Move teardown, pause and resume into services/lifecycle.rs
- **deployments:** Move cancellation into services/cancel.rs

### Testing

- Replace bare #[ignore] with runtime skips or stated reasons
- **sandbox:** Read the agent-runtime pin through the workspace table
- **backup:** Allow expect in the feature-gated MariaDB PITR end-to-end test
- **backup:** Allow expect in the feature-gated workload job proof test
- **network:** Allow expect in the Linux kernel integration tests

## [0.1.0-nightly.20261031.031aafcf3] - 2026-10-30

### Added

- **presets:** Offer Node builds and skip build inputs in repo detection ([#1672](https://github.com/gotempsh/temps/issues/1672))
- **analytics:** Exclude datacenter traffic, referrer spam and user-defined traffic ([#1673](https://github.com/gotempsh/temps/issues/1673))

### CI

- Cut PR wall time with an e2e smoke gate and parallel tests ([#1679](https://github.com/gotempsh/temps/issues/1679))

### Fixed

- **deps:** Patch security vulnerabilities reported by Dependabot
- **firecracker:** Say why micro_vm deployments are refused ([#1676](https://github.com/gotempsh/temps/issues/1676))

## [0.1.0-nightly.20261030.5a305eadc] - 2026-10-30

### Added

- **status-page:** Platform-wide status pages with groups and a redesigned public page ([#1674](https://github.com/gotempsh/temps/issues/1674))

### Fixed

- **notifications:** Re-enter credentials when a provider's destination changes ([#1670](https://github.com/gotempsh/temps/issues/1670))
- **deps:** Patch Dependabot security alerts (2026-10-29) ([#1669](https://github.com/gotempsh/temps/issues/1669))

## [0.1.0-nightly.20261029.66ae40150] - 2026-10-29

### Added

- **domains:** Switch certificate validation and configure renewal alerts ([#1664](https://github.com/gotempsh/temps/issues/1664))
- **reachability:** HTTPS only, on standard ports, for tunnel addresses ([#1666](https://github.com/gotempsh/temps/issues/1666))

### Documentation

- **readme:** Simplify the README and show a screenshot for every feature ([#1668](https://github.com/gotempsh/temps/issues/1668))

### Fixed

- **skills:** Verify the installer against temps.sh's published checksum ([#1667](https://github.com/gotempsh/temps/issues/1667))

## [0.1.0-nightly.20261029.83ba21d68] - 2026-10-29

### Added

- **deployments:** Run Docker Compose stacks on worker nodes with a static home ([#1658](https://github.com/gotempsh/temps/issues/1658))
- **workload:** ADR-055 workload plane and runtime drivers ([#1632](https://github.com/gotempsh/temps/issues/1632))
- **web:** Make it reachable is a page that asks what is possible first ([#1665](https://github.com/gotempsh/temps/issues/1665))

### CI

- **deps:** Pin the zstd 0.13.3 Pingora exception to the APIs it calls ([#1663](https://github.com/gotempsh/temps/issues/1663))

### Fixed

- **auth:** Require verified Cloud email and keep OIDC state out of logs ([#1662](https://github.com/gotempsh/temps/issues/1662))
- **deployments:** Honor Dockerfile command and entrypoint overrides ([#1661](https://github.com/gotempsh/temps/issues/1661))

## [0.1.0-nightly.20261028.b1f12c448] - 2026-10-28

### Added

- **metrics:** Collect metrics for managed MariaDB services ([#1655](https://github.com/gotempsh/temps/issues/1655))
- **shield:** Selective mode, workers, signatures, fleet learning and netwatch (ADR 066 phases 4–8) ([#1653](https://github.com/gotempsh/temps/issues/1653))

### Fixed

- **websocket:** Validate Origin on terminal and conversation sockets ([#1654](https://github.com/gotempsh/temps/issues/1654))
- **cloud-client:** Reconnect the heartbeat promptly after a proxy drop ([#1657](https://github.com/gotempsh/temps/issues/1657))
- **mesh:** Explain the cluster CA blocker and let an admin without MFA rotate it ([#1656](https://github.com/gotempsh/temps/issues/1656))
- **web:** Render Shield proposals inside the settings page container ([#1660](https://github.com/gotempsh/temps/issues/1660))

## [0.1.0-nightly.20261027.2ac987440] - 2026-10-26

### Added

- **logs:** Per-container log ingestion budget with throttled-container warnings ([#1629](https://github.com/gotempsh/temps/issues/1629))
- **network:** Configurable overlay network name; temps join checks the host first
- **web:** Put worker ingress up front
- **cloud:** A hosted plane's bootstrap file can switch telemetry export on, once
- **web:** Set up worker ingress from one "Expose your apps" page
- **proxy:** Let a project serve its own /api/_temps and /api/otel
- **metrics:** Show worker proxy and host metrics in the console (ADR 061)
- **web:** Show what the Cloudflare token needs wherever a tunnel is set up
- **entities:** Per-address audience and reachability plans and runs
- **proxy:** Refuse a private-only allow list where visitors come through a proxy
- **ingress:** Say whether each address is reachable, and make it reachable
- **web:** Domains is part of the project, not its settings
- **web:** Make it reachable from the header, domains and a finished deploy
- **dns:** Records carry the run that wrote them and can be read back
- **ingress:** A tunnel being removed is never synced or recreated
- **reachability:** Journal DNS changes, reconcile safely, undo only what the run wrote
- **web:** Tunnel token scope note and reachability conflict decisions
- **entities:** Store each private address's range, worker and listener
- **agent:** Private ingress mode, bound only to a private address
- **routes:** Private addresses reach only their worker's private listener
- **agent:** Serve private routes in range and check them from inside
- **reachability:** Make an address reachable only from my private network
- **ingress:** Auto_publish never publishes a private-only hostname
- **nodes:** Show a worker's private listener and refuse enabling it publicly
- **web:** Only my private network
- **reachability:** Preview-domain status, primary address and reachability warning
- **reachability:** Tunnel environment hosts
- **reachability:** Serve private addresses at a worker's tailnet name (ADR 065 3a)
- **reachability:** Tailscale Services as a way in (ADR 065 3b)
- **reachability:** Only these tailnet users (ADR 065 3c)
- **shield:** Reputation lists in log mode (ADR 066 phase 2) ([#1638](https://github.com/gotempsh/temps/issues/1638))
- **shield:** JA4 ClientHello parser (ADR 066 phase 3) ([#1639](https://github.com/gotempsh/temps/issues/1639))
- **shield:** Record JA4 at the control plane, local proxies and workers (ADR 066 phase 3) ([#1641](https://github.com/gotempsh/temps/issues/1641))
- **reachability:** Signed-in addresses behind the Temps gate (ADR 065 phase 4a)
- **reachability:** Cloudflare Access for tunneled signed-in addresses (ADR 065 phase 4b)

### CI

- **metrics:** Run temps-metrics tests in the docker-deployments group

### Documentation

- **temps:** Prefer curated services before custom deployments ([#1630](https://github.com/gotempsh/temps/issues/1630))
- **adr:** 065 tailnet names, tailnet users and signed-in addresses ([#1624](https://github.com/gotempsh/temps/issues/1624))
- **adr:** 063 sandboxes on a control plane that runs no workloads
- **adr:** 064 making a project reachable: who first, then how
- **adr:** 064 address review: bound plans, crash-safe runs, scoped tokens and checks
- **adr:** 064 implementation notes for phases 0 and 1
- **adr:** 064 address second review: DNS edits after planning, ambiguous lookups, tunnel scope
- **adr:** 064 address third review: changing an existing DNS record
- **adr-064:** A tunnel removed by undo is never recreated by sync
- **adr-064:** Tunnel undo touches only journaled DNS records
- **adr-064:** Implementation notes for the phase 1 review fixes
- **adr-064:** Implementation notes for phase 2
- **adr:** 068 environment addresses without a public preview domain

### Fixed

- **metrics:** Stop latest-value reads scanning a source's full history
- **agent:** Install the cloud-metadata egress guard at start
- Route hosted planes' trace queries, sample the host in console-only mode, guide least-privilege Cloudflare tokens
- **nodes:** Re-join by plane identity with proof, remove an empty node in one step
- **join:** A re-join keeps the overlay name the node already runs
- **sandbox:** Serve sandboxes on a control plane that runs no workloads
- **web:** Node Sandboxes tab never shows a bare "Not Found"
- **cli:** Classify plane_identity for the assistant: a node-agent endpoint
- **reachability:** Clippy: box large journal variants, merge identical probe branches
- **reachability:** List agent ingress private in the CLI inventory; AA contrast for token and step states
- **web:** Keep HTTP-01 when port 80 is not seen; e2e follow Domains in the nav
- **routes:** Serve only a private audience to its range; an unknown one reaches nobody
- Address the security review and CodeRabbit review of #1626
- **join:** Send the saved token only to a proven control plane; private ingress ports
- **reachability:** Only a way in never set up reads as no_address
- **reachability:** Guard tunnel environment hosts against takeover and duplicates
- **reachability:** Address review findings on primary addresses and episodes
- **status-page:** Probe through the local proxy when there is no public address
- **agent:** Pass on only the identity headers tailscaled sets
- **routes:** Build the private_via test fixture and satisfy clippy (ADR 065 3a)
- **reachability:** Never delete a Service that lost its marker; no redirects to Tailscale
- **agent:** Keep a rebound Serve socket, refuse lone --tailscale-serve, re-check AdvertiseServices
- **reachability:** Never let a rolled-back agent serve tailnet_users addresses
- **reachability:** Check the Service host's tailnet; report a Service still advertised
- **agent:** Say why a tagged device shared in from another tailnet is refused
- **proxy:** Sign attack-mode challenges (ADR 066 phase 1) and add ADR 066 Shield ([#1633](https://github.com/gotempsh/temps/issues/1633))
- **status-page:** Probe as before and show no_address only for an unresolved name
- Log ingestion, ClickHouse write amplification and node disk health (#1613-#1620) ([#1637](https://github.com/gotempsh/temps/issues/1637))
- **reachability:** Tailnet review follow-ups (ADR 065)
- **agents:** Scope autofixer error groups to the requested project ([#1647](https://github.com/gotempsh/temps/issues/1647))
- **deps:** Drop unmaintained ttf-parser by reading PDFs with lopdf 0.46
- **ai-chat:** Bound total PDF decompression per document
- **reachability:** Address the security review of the signed-in gate (ADR 065 phase 4a)
- **console:** Classify the gate and Cloudflare Access operations for the assistant
- **reachability:** Address the security review of Cloudflare Access (ADR 065 phase 4b)
- **reachability:** Never let a rolled-back agent serve signed-in addresses open
- **reachability:** Type the Cloudflare Access token as a secret
- **reachability:** Withhold keys of held-back signed-in hosts and trust only real gate answers
- **reachability:** Drop gate cookies on every request and trust gate markers only from Temps
- **reachability:** Close the gaps CodeRabbit found in signed-in addresses
- **reachability:** Offer environment hosts only to a public plan
- **security:** Refresh release packages and gate image channel promotion ([#1651](https://github.com/gotempsh/temps/issues/1651))

### Miscellaneous

- **cli:** Verify staged npm tarballs before approving them ([#1631](https://github.com/gotempsh/temps/issues/1631))
- **console:** State why the AI assistant cannot publish environment hosts
- **cli:** Regenerate the API client from the merged spec
- **api:** Regenerate the OpenAPI spec and clients from the merged server

### Styling

- Rustfmt the sandbox and preview-gateway changes
- Rustfmt the ADR 064 files from phases 0 and 1

### Testing

- **database:** Stop the SQL logging test losing events to parallel tests ([#1627](https://github.com/gotempsh/temps/issues/1627))
- **e2e:** Domains is in the project navigation, not under Settings

## [0.1.0-nightly.20261025.3433dfeb3] - 2026-10-25

### Added

- **sandbox:** Operator limits and per-user cap on sandbox create
- **metrics:** Worker node metrics and CPU/memory/disk alerts ([#1611](https://github.com/gotempsh/temps/issues/1611))

### CI

- **rust-tests:** Raise the musl build timeout for a cold cargo cache
- **rust-tests:** Document the measured musl build time

### Documentation

- Add REVIEW.md and an optional sccache/S3 build cache guide
- Address review feedback on REVIEW.md and the sccache guide
- Clarify sccache server restarts and mark BASEDIRS as unverified for Rust
- Qualify sccache restart and parallel-agent cache hit guidance
- **adr:** 064 making a project reachable: who first, then how
- **adr:** 064 address review: bound plans, crash-safe runs, scoped tokens and checks
- **adr:** 064 address second review: DNS edits after planning, ambiguous lookups, tunnel scope
- **adr:** 064 address third review: changing an existing DNS record
- **adr-064:** A tunnel removed by undo is never recreated by sync
- **adr-064:** Tunnel undo touches only journaled DNS records

### Fixed

- **sandbox:** Cap caller-supplied cpu, memory, pids and disk limits on create
- **sandbox:** Reject cpu and memory limits Docker cannot enforce
- **deps:** Patch ctutils and dompurify advisories
- **sandbox:** Enforce operator limits on provider defaults and scope the user cap
- **sandbox:** Cap an omitted memory limit at the host's memory
- **deps:** Patch crypto-bigint and happy-dom advisories
- **sandbox:** Bound the nested resources fields in the create schema
- **config:** Add lower bounds to the sandbox ceiling settings schemas
- **database:** Retry transient failures when building post-migration indexes
- **network:** Admit VXLAN only from known peers and set its local address
- **network:** Check the peer's route and scope the VXLAN drop to its VNI

### Testing

- **network:** Inject VXLAN only from test-built namespaces

## [0.1.0-nightly.20261023.db8681afe] - 2026-10-23

### Added

- **web:** Group a variable's environments and mark deleted ones
- **web:** Group and filter the environment picker in variable dialogs
- **ai-gateway:** Meter chat turns, audit provider-key changes, and expose a setup path
- **ai-chat:** Add a gateway-backed Temps assistant conversation
- **web:** Add the Temps assistant chat with simple and advanced modes
- **ai-chat:** Let assistant changes run without asking, with an opt-in to confirm
- **web:** Add an "Ask before changes" switch to the assistant composer
- **web:** Make the Temps assistant a full-screen mode
- **web:** Show the assistant in a focused layout without the console chrome
- **web:** Size the full-screen assistant onboarding to fill the page
- **web:** List assistant chats in a left sidebar on the full-screen page
- **ai-chat:** Teach the assistant to get a user started on the platform
- **ai-gateway:** Expose model catalogue details and prefer a known default
- **web:** Pick the assistant model per chat from the prompt input
- **ai-chat:** Give the assistant the operations it needs to run the platform
- **web:** Keep the assistant composer calm on focus
- **ai-chat:** Show users the assistant's API tool results, redacted and capped
- **ai-chat:** Title chats from the conversation and tint assistant chats in the list
- **ai-chat:** Let the assistant keep watching what it starts and self-heal it
- **web:** Show what the assistant is watching and fixing in the chat
- **ai-chat:** Let assistant chats take images and text files, by what the model can read
- **web:** Attach images and text files in assistant chats, by what the model reads
- **ai-chat:** Accept PDFs and other binary files in assistant chats
- **web:** Attach files on the first message and accept PDFs and other files
- **web:** Resizable chat list on the assistant page, responsive start screen
- **web:** Make a URL in backticks a link in assistant replies
- **ai-chat:** Tell the console which deployment a goal watches
- **web:** Show a chat's deployments in a side panel and drop finished goals
- **web:** Chats and work views for the full-screen assistant
- **web:** Open a project named in an assistant reply beside the chat
- **git:** Let the assistant create repositories through the user's own connection
- **git:** Let the assistant commit files and open pull requests through the user's connection
- **ai-chat:** Show what a chat has cost and price gateway usage from the model catalogue
- **ai-chat:** Link a sandbox to an assistant chat
- **web:** Link a sandbox to an assistant chat
- **ai-chat:** Link a sandbox to a project chat
- **ai-chat:** Let the assistant create the chat's sandbox on demand
- **web:** Show a chat's project logo and sandbox backend in the chat list
- **ai-chat:** Keep the assistant on Temps for previews and hosting
- **ai-chat:** Teach the assistant tasks through markdown playbooks
- **ai-chat:** Teach the assistant to connect a linked service to a project
- **ai-chat:** List what a chat made in a Made here tab beside it
- **ai-chat:** Show the sandbox's running app in a Preview tab beside the chat
- **sandbox:** Give a sandbox its own PostgreSQL database and scoped role
- **ai-chat:** Let a chat's sandbox use a project's database with its own credentials
- **ai-chat:** Connect a chat's sandbox to a project's databases from the UI
- **ai-chat:** List a service the chat linked to a project in Made here
- **ai-chat:** Deploy a chat sandbox's files as a project's source without Git
- **sandbox:** Link sandboxes to services like projects, with per-service connect
- **sandbox:** Scoped access to Redis, MariaDB and MongoDB services
- **ai-chat:** Start a chat's sleeping sandbox when the chat opens and when a turn starts
- **ai-chat:** Show how many tokens a chat's context holds
- **ai-gateway:** Cache the fixed prompt prefix and record cached tokens
- **ai-chat:** Summarize long chats and trim old tool output in batches
- **web:** Offer "Compact now" and show how much of a chat is summarized
- **ai-chat:** Log which sections of the system prompt are largest
- **web:** Show model vendor logos and vendor tabs in the model picker
- **web:** List the popular models first in the model picker
- **ai-chat:** Send workspace chat images to gateway models as native image parts
- **web:** Press / to focus the assistant message box
- **web:** Add a Terminal tab to the assistant's working panel
- **web:** Focus the assistant on the conversation
- **ai:** Switch assistant chats between gateway providers
- **web:** Keep the assistant chat tailed and its composer usable when narrow
- **nodes:** Join workers with no public address and a compute pool that avoids collisions
- **nodes:** Remove a worker node that is gone, even while it still serves
- **nodes:** Add a worker in three steps and confirm the pool once
- **ingress:** Publish apps on workers through a tunnel, one list of domains
- **web:** Let operators pick a project scope when adding an agent secret (F1)
- **projects:** Install a GitHub repository webhook from the console ([#1596](https://github.com/gotempsh/temps/issues/1596))
- **auth:** Let admins reset another user's MFA ([#1590](https://github.com/gotempsh/temps/issues/1590))

### CI

- **sentry:** Raise Sentry SDK Acceptance timeout to 40 minutes

### Documentation

- **auth:** Any role can enroll MFA; recovery codes work at sign-in
- **adr:** Propose a common workload model for deployed containers and sandboxes
- **adr:** Add a migration and rollback section to the workload model
- **adr:** Propose autofix incidents in workspaces (ADR-054)
- Require Temps to run on 2 vCPU/4 GB, and on 1 vCPU/2 GB with an external database
- **adr:** Propose a workload plane with pluggable runtime drivers
- **skills:** Drop the removed data ai-access command from the CLI references

### Fixed

- **domains:** Record the validated challenge as verification_method on ACME issuance
- **environments:** Ignore deleted environments when updating env vars
- **web:** Never strip a variable's links when saving the edit dialog
- **vulnerability-scanner:** Enforce project access on by-id scan handlers
- **providers:** Check service ownership on masked env preview
- **providers:** Create a Postgres HA cluster without a member list
- **web:** Stop offering file uploads that assistant chats always refuse
- **ai-chat:** Read the per-conversation attachment total as a BIGINT
- **web:** Open the project picker from the chat sidebar while a chat is open
- **web:** Leave batch-only models out of the assistant model picker
- **web:** Show the app's outline while it loads instead of a bare spinner
- **web:** Make the chat composer compact on phones
- **web:** Make Enter a new line in the chat composers on phones
- **web:** Keep dropdowns and pickers on screen on phones
- **web:** One-line assistant header on phones that stays visible while typing
- **web:** Declutter the chat composer toolbar
- **web:** Open the new chat after the first message instead of returning to the start screen
- **web:** Use the project list's icons in the assistant's project cards
- **ai-chat:** Resume a chat's sleeping sandbox and survive a cancelled create
- **ai-chat:** Give the assistant the sandbox's real preview URL
- **sandbox:** Reach cluster PostgreSQL members on their own port
- **ai-api-tools:** Type list body fields as arrays so commit_repository_files accepts files
- **sandbox:** Leave a legacy MinIO-server S3 service out of sandbox access
- **web:** Animate the initial load screen so it no longer looks frozen
- **web:** Drop the empty gutter beside model rows and wrap the vendor tabs
- **web:** Keep the model picker usable on a phone
- **web:** Do not show a sent message twice when a refresh beats its live echo
- **backup:** Resolve the HA PostgreSQL cluster primary through the monitor
- **ci:** Clear clippy, eslint and the cluster retry test on the assistant branch
- **ai-chat:** Resolve clippy findings in the assistant crate
- **deps:** Bump rustls-webpki and webpki-roots past the emSign distrust advisories
- **ai:** Keep provider keys off half-typed base URLs and Gemini paths out of model names
- **ai:** Close security gaps in the assistant's sandbox, git, upload and autofix paths
- **ai:** Drop sandbox links to projects the user lost and keep autofix edits per project
- **sandbox:** Give a stopped sandbox's Docker network back to the pool
- **ai:** Re-check project access in links, autofix and sandbox credentials
- **ci:** Classify new operations for the assistant and steady flaky checks
- **ai-chat:** Clear the clippy errors hidden behind the providers failure
- **autofix:** Never undo newer edits or offer a partial patch
- **ai-chat:** Box the sandbox error carried by link failures
- **agents:** Scope agent secrets to projects and redact them from run logs (F1)
- **import:** Restrict the host Docker import source to instance admins (F2)
- **ai-gateway:** Require operator standing and project access for limit writes (F8)
- **auth:** Revoke every session when a password is reset (F3)
- **import:** Require the create permissions of what an import makes (F7)
- **backup:** Only restore from a completed backup (F4)
- **deployments:** Refuse pause/resume outside their source state (F5)
- **deployments:** Never revive a deleted environment on branch push (F6)
- **deployments:** Tear down containers when promote/rollback completion fails (F9)
- **environments:** Enforce one env var per key and scope (F10)
- **revenue:** Project subscription state in event order (F11)
- **revenue:** Bound imported amounts and saturate MRR totals (F12)
- **backup:** Require an instance admin for instance-wide schedules (NV1)
- **revenue:** Keep invoice-realized MRR out of the event-order fence (F11)
- **agents:** Redact JSON-escaped secrets and forget them when a run ends (F1)
- **web:** Keep an agent secret's scope when editing or deleting it (F1)
- **agents:** Re-register secrets before each autofixer sandbox phase (F1)
- **api:** Mark the secret delete scope parameter as a plain integer (F1)
- **deployments:** Let project deletion remove its own legacy-labelled containers
- **migrations:** Skip orphaned error events when backfilling error_group_fingerprints
- **database:** Warn instead of refusing to start on a newer schema
- **providers:** Drop the removed ai_data_access field from a test fixture
- **cli:** Report a newer schema in migrate and doctor, not as an upgrade
- **nodes:** Address review of the outbound join and cluster network form
- **ingress:** Close review gaps in token revocation, recovery and heartbeats
- **web:** Read every tunnel page and show the saved ingress switch
- **web:** Pass the web lint on the worker ingress screens
- **ingress:** Look up tunnel zones in one query, drop an expect
- **ingress,agent:** Rebuild a moved overlay, clean up half-made tunnels, audit failed creates
- **network:** Never move an allocation services hold addresses in
- **network:** Report service-held allocations in the pool status
- **nodes:** Type the mesh enroll join token as a secret
- **agent:** Stream connector downloads with a size cap
- **nodes:** Save a worker's ingress settings in one transaction
- **ingress:** Bound the number of tunnels a cluster can have
- **monitoring:** Report missing checks and samples as gaps, not uptime
- **agents:** Keep autofixer redaction until an in-flight phase ends on cancel (F1)
- **migrations:** Seed the revenue fence from lifecycle events only (F11)
- **ai-chat:** Stop read-only keys changing their owner's conversations ([#1599](https://github.com/gotempsh/temps/issues/1599))
- **git:** Register a repository webhook for GitHub PAT connections ([#1596](https://github.com/gotempsh/temps/issues/1596))
- **git:** Keep GitHub repository webhooks unique per project
- **deps:** Patch Next.js example and pin certifi for security alerts
- **deps:** Patch Next.js in deployment and preset test fixtures
- **ai:** Withhold the admin MFA reset from the assistant ([#1590](https://github.com/gotempsh/temps/issues/1590))
- **web:** Keep the AI gateway key dialog within the viewport

### Miscellaneous

- **api:** Regenerate OpenAPI clients for the assistant conversation endpoint
- **api:** Regenerate clients for the assistant goal endpoints
- **api:** Regenerate clients for the attachment upload responses
- **api:** Regenerate clients for the attachment upload responses
- **api:** Regenerate clients for chat sandbox project connections
- **ai-chat:** Checkpoint workspace assistant work in progress
- **ai-chat:** Checkpoint workspace assistant and autofix work in progress
- **api:** Sync the OpenAPI spec and clients with the audit fixes

### Performance

- **web:** Stop re-rendering the whole transcript on every streamed token

### Refactor

- **sandbox:** Give each service type its own sandbox access engine, add S3
- **web:** Filter the model picker with a vendor dropdown, drop name prefixes
- **database:** Share the newer-schema apply path
- **web:** Build the tunnel domain page on the Detail template

### Styling

- **ai-chat:** Apply rustfmt
- **migrations:** Apply rustfmt
- **sandbox:** Wrap the pool-exhaustion test assertions for rustfmt
- **plugins:** Rustfmt the worker plugin build path

### Testing

- **web:** Set the fake window in the last-model tests without assigning it
- **auth:** Pass the OIDC test client secret as a SecretString
- **web:** Cover the full-screen assistant entry and the dock gate
- **backup:** Build the NV1 scope test service without a Docker socket (NV1)
- **agents:** Cover the autofixer secret-redaction lifecycle end to end (F1)
- **database:** Concurrent starts on a newer schema keep the ledger consistent
- **upgrade:** Expect a warning, not a refusal, on a newer schema
- **cli:** List the agent ingress commands in the audited inventory
- **providers:** Drop removed ai_data_access field from service fixture
- **ai-chat:** Drive the #1599 routes through their real handlers
- **ai-chat:** Cover authentication, scoping and races on the #1599 routes
- **ai-chat:** Pin archive and restore as idempotent, guarded soft changes

## [0.1.0-nightly.20261021.507360e6f] - 2026-10-21

### Added

- **sentry:** Ingest SDK telemetry through native Temps services
- **sentry:** Complete native ingestion adapters and Rust acceptance coverage

### Fixed

- **web:** Stop failed DOM assertions from stalling the test run ([#1581](https://github.com/gotempsh/temps/issues/1581))
- **deps:** Bump aiohttp past GHSA-5wr8-gcqf-gqf3 and GHSA-2g87-pp6c-29x5 ([#1580](https://github.com/gotempsh/temps/issues/1580))
- **security:** Address findings from authorized security assessment ([#1583](https://github.com/gotempsh/temps/issues/1583))
- **auth:** Bind the OIDC callback to the browser that started the login ([#1585](https://github.com/gotempsh/temps/issues/1585))
- **sentry:** Stabilize grouping and isolate retry identities
- **deps:** Refresh patched TLS verifier and root bundles
- **auth:** Cap deployment token permissions at the caller's own

### Testing

- **dev-cluster:** Send the Secure session cookie to the loopback control plane ([#1586](https://github.com/gotempsh/temps/issues/1586))
- **sentry:** Validate SDK message capture stacks
- **sentry:** Cover native replay initialization regressions
- **cluster:** Preserve secure sessions in runtime health acceptance
- **auth:** Check the token grant mapping in both directions
- **auth:** Cover the token ceiling's create and update rules and error body

## [0.1.0-nightly.20261020.aedf89c1d] - 2026-10-19

### Added

- **cli:** Support external TLS and admin password files in setup ([#1569](https://github.com/gotempsh/temps/issues/1569))

### CI

- Key the nextest target seed on Cargo.lock and rustc ([#1576](https://github.com/gotempsh/temps/issues/1576))

### Fixed

- **web:** Show a single toast when a mutation fails ([#1568](https://github.com/gotempsh/temps/issues/1568))
- **analytics:** Honor ClickHouse event and visitor retention ([#1575](https://github.com/gotempsh/temps/issues/1575))
- **auth:** Harden API key role bounds, second-factor checks and login timing ([#1577](https://github.com/gotempsh/temps/issues/1577))

## [0.1.0-nightly.20261019.2bd02ef7f] - 2026-10-19

### Added

- **health:** Add worker probes and distributed runtime monitoring ([#1565](https://github.com/gotempsh/temps/issues/1565))

### Fixed

- **cloud:** Let cloud-oidc.json bootstrap consent and provision on a hosted plane ([#1566](https://github.com/gotempsh/temps/issues/1566))

## [0.1.0-nightly.20261018.d438f7b09] - 2026-10-17

### Added

- **web:** Add Plugins and Email to the main sidebar ([#1561](https://github.com/gotempsh/temps/issues/1561))
- **cli:** Export server traces over OTLP (OpenTelemetry) ([#1563](https://github.com/gotempsh/temps/issues/1563))

### CI

- **deps:** Ignore zip 8.6.0 advisories blocked on upstream crates ([#1559](https://github.com/gotempsh/temps/issues/1559))

### Fixed

- **web:** Add Docker Compose upgrade guidance to warnings ([#1560](https://github.com/gotempsh/temps/issues/1560))
- **deployer:** Apply build limits live without restarting temps serve ([#1562](https://github.com/gotempsh/temps/issues/1562))
- **web:** Make the Projects page and shared layout usable on phones ([#1564](https://github.com/gotempsh/temps/issues/1564))

## [0.1.0-nightly.20261017.3c3dde4e0] - 2026-10-17

### Added

- **deployments:** Bound the builder queue and fall back when a builder stays busy ([#1551](https://github.com/gotempsh/temps/issues/1551))
- **providers:** Pluggable S3-compatible managed storage (RustFS and Garage) ([#1552](https://github.com/gotempsh/temps/issues/1552))
- **ai-gateway:** Per-project rate limits, monthly budgets and cost attribution ([#1554](https://github.com/gotempsh/temps/issues/1554))
- **deployments:** Configure builder limits and dispatch verified CI builds ([#1558](https://github.com/gotempsh/temps/issues/1558))

### Fixed

- **sandbox:** 409 instead of 404 when a sandbox's VM is down; reconcile lost VMs ([#1550](https://github.com/gotempsh/temps/issues/1550))
- **sandbox:** Answer 409 when a sandbox's VM is down, reconcile lost VMs, keep VMs across service restarts ([#1553](https://github.com/gotempsh/temps/issues/1553))
- **operations:** Stop counting autofix reviews as running ([#1557](https://github.com/gotempsh/temps/issues/1557))

## [0.1.0-nightly.20261016.0a7de042e] - 2026-10-16

### Added

- **tls:** Auto-provision environment URLs over HTTP-01 when no wildcard covers them ([#1518](https://github.com/gotempsh/temps/issues/1518))
- **sandbox:** Warn at startup when a guest agent is from a different build than temps ([#1536](https://github.com/gotempsh/temps/issues/1536))
- **deployments:** Scan and read image config on the build node instead of copying the image back ([#1545](https://github.com/gotempsh/temps/issues/1545))
- **deployer:** Make standard Compose idioms work and add real diagnostics ([#1540](https://github.com/gotempsh/temps/issues/1540))

### Documentation

- **architecture:** Design multi-node placement and CI build dispatch; add build result verification ([#1548](https://github.com/gotempsh/temps/issues/1548))

### Fixed

- **web:** Refetch renewal history after provisioning; invalidate project lists after template create ([#1496](https://github.com/gotempsh/temps/issues/1496))
- **web:** Keep visible lists fresh with opt-in focus refetch and background polling ([#1519](https://github.com/gotempsh/temps/issues/1519))
- **sandbox:** Answer exhausted Docker address pools with 503, release leaked networks, repair unpublished image tags ([#1520](https://github.com/gotempsh/temps/issues/1520))
- **web:** Gate row actions by role and use the compact empty state ([#1521](https://github.com/gotempsh/temps/issues/1521))
- **mesh:** Manage the mesh link in-process so it needs only CAP_NET_ADMIN ([#1526](https://github.com/gotempsh/temps/issues/1526))
- **web:** Keep the query string and hash on the remaining legacy redirects ([#1535](https://github.com/gotempsh/temps/issues/1535))
- **web:** Show the authored error title and server reason for failed actions ([#1532](https://github.com/gotempsh/temps/issues/1532))
- **web:** Give workspaces, notifications, routes and logs actionable empty states ([#1533](https://github.com/gotempsh/temps/issues/1533))
- **web:** Keep typed characters in the environment variable filter ([#1530](https://github.com/gotempsh/temps/issues/1530))
- **web:** Validate notification route, SSO provider and backup schedule create forms inline ([#1534](https://github.com/gotempsh/temps/issues/1534))
- **web:** Name icon-only buttons and un-nest span waterfall toggles ([#1531](https://github.com/gotempsh/temps/issues/1531))
- **web:** Let the environment variable detail page reveal regular values ([#1541](https://github.com/gotempsh/temps/issues/1541))
- **analytics:** Give non-admins the configured external URL in the install snippet ([#1538](https://github.com/gotempsh/temps/issues/1538))
- **web:** Make scrolling code reachable by keyboard and fix badge and field a11y ([#1547](https://github.com/gotempsh/temps/issues/1547))
- **web:** Explain empty and loading states on the remaining #1504 routes ([#1546](https://github.com/gotempsh/temps/issues/1546))
- **metrics:** Sum per-interval proxy counters per bucket instead of averaging ([#1544](https://github.com/gotempsh/temps/issues/1544))
- **projects,environments:** Finish project delete after client disconnect; audit env var writes ([#1543](https://github.com/gotempsh/temps/issues/1543))
- **cli,web,proxy,domains:** Non-API 2xx replies, tied-row paging and sort, structural a11y guard ([#1549](https://github.com/gotempsh/temps/issues/1549))

### Miscellaneous

- **release:** @temps-sdk/cli 0.1.38, plugin 0.1.0-beta.2, analytics SDK fixes, repin skills ([#1525](https://github.com/gotempsh/temps/issues/1525))

### Testing

- **web:** Follow the shared delete dialog in the project-group lifecycle e2e ([#1529](https://github.com/gotempsh/temps/issues/1529))

## [0.1.0-nightly.20261015.bf0d088cf] - 2026-10-15

### Added

- **proxy:** Per-project and per-environment IP allow/deny lists ([#1493](https://github.com/gotempsh/temps/issues/1493))
- **project-groups:** Connect a repository on its own page ([#1495](https://github.com/gotempsh/temps/issues/1495))
- **sandbox:** Refresh guest agents safely and allow Docker in microsandbox ([#1494](https://github.com/gotempsh/temps/issues/1494))

### Fixed

- **sandbox:** Make microsandbox and firecracker setup land where the server reads ([#1492](https://github.com/gotempsh/temps/issues/1492))
- Analytics retention, sandbox files, console accessibility and refused-read handling ([#1491](https://github.com/gotempsh/temps/issues/1491))
- **web:** Confirm every destructive action and name its target ([#1514](https://github.com/gotempsh/temps/issues/1514))
- **web:** Report failed reads, show not-found states and explain empty ones ([#1517](https://github.com/gotempsh/temps/issues/1517))
- **web:** Keep search text in the URL and stop dropping keystrokes ([#1515](https://github.com/gotempsh/temps/issues/1515))
- **web:** Open comboboxes with ArrowDown and keep narrow layouts and contrast accessible ([#1516](https://github.com/gotempsh/temps/issues/1516))
- **web:** Disable write controls and explain refused reads for restricted users ([#1513](https://github.com/gotempsh/temps/issues/1513))

## [0.1.0-nightly.20261014.584f5b72d] - 2026-10-14

### Added

- **project-groups:** Group projects under a parent with shared env, routes and hosting ([#1473](https://github.com/gotempsh/temps/issues/1473))
- **serve:** Read --disable-self-update and --pre-migration-backup from the environment ([#1487](https://github.com/gotempsh/temps/issues/1487))

## [0.1.0-nightly.20261014.b172728cb] - 2026-10-14

### Added

- **agents:** Sign AI CLIs in to a subscription from the console ([#1484](https://github.com/gotempsh/temps/issues/1484))
- **sandbox:** Interactive terminals on the firecracker and microsandbox backends ([#1480](https://github.com/gotempsh/temps/issues/1480))
- **ai-chat:** Terminal threads in the AI workspace ([#1485](https://github.com/gotempsh/temps/issues/1485))

### Fixed

- **deployments:** Classify static-site deploy failures instead of reporting deploy/unknown ([#1476](https://github.com/gotempsh/temps/issues/1476))
- **providers:** Retry pg_autoctl config changes the keeper rewrites during the SCRAM upgrade ([#1479](https://github.com/gotempsh/temps/issues/1479))
- **clickhouse:** Pin the wire codec so a ZSTD-default server no longer breaks every query ([#1477](https://github.com/gotempsh/temps/issues/1477))
- **deployer:** Skip non-publishable static entries instead of failing the deployment ([#1481](https://github.com/gotempsh/temps/issues/1481))
- **release:** Ship temps-vm-agent in the linux release tarballs ([#1478](https://github.com/gotempsh/temps/issues/1478))
- **providers:** Stop the docker-providers CI hang and fail stuck tests fast ([#1486](https://github.com/gotempsh/temps/issues/1486))

### Testing

- **web:** Update project navigation e2e for the Status pages monitoring link ([#1475](https://github.com/gotempsh/temps/issues/1475))

## [0.1.0-nightly.20261013.05ff10cc8] - 2026-10-13

### Added

- **ai-gateway:** Self-hosted OpenAI-compatible providers routed by catalog
- **web:** Add self-hosted OpenAI-compatible providers to the AI gateway console
- **cli:** Add temps start and temps stop to run Temps locally
- **status-page:** Publish public status pages for selected monitors
- **web:** Manage public status pages and incident update visibility
- **cli:** Add status-pages commands and public incident updates
- **deployments:** Build mixed-architecture deployments natively per worker
- **status-page:** Serve status pages on verified custom domains
- **web:** Manage status page custom domains in the console
- **cli:** Add status-pages domains commands
- **cli:** Add status-pages domains renew

### CI

- **deps:** Document microsandbox-only bincode exception ([#1456](https://github.com/gotempsh/temps/issues/1456))
- **images:** Publish managed-database images to GHCR and gate releases on them ([#1462](https://github.com/gotempsh/temps/issues/1462))

### Documentation

- **status-page:** Document publishing public status pages
- **status-pages:** Document custom domains
- **status-pages:** Document custom domain certificate renewal

### Fixed

- **backup:** Send plain SigV4 uploads to S3-compatible stores
- **backup:** Send Content-MD5 on every upload
- **ai-gateway:** Close review gaps in self-hosted provider routing
- **ai-gateway:** Explain how to trust untrusted overlay-network provider addresses
- **ai-gateway:** Keep configured provider hosts out of native endpoint errors
- **ai-gateway:** Compare provider destinations by path case and keep listed model ids
- **web:** Keep the self-hosted provider fields mounted while switching providers
- **web:** Track provider key settings saves per key
- **cli:** Leave hidden files out of static deploy bundles
- **cli:** Apply the server's full publish policy to static bundles
- **cli:** Make temps start and stop safe to overlap, reset and retry
- **cli:** Prove volume ownership and remove the lock takeover race
- **cli:** Suggest bunx for the skills fallback in temps start
- **cli:** Install only the temps skill in temps start
- **status-page:** Close takedown races and bound public incident reads
- **web:** Keep unsaved status page edits and page through incidents
- **web:** Lock the status page form while saving and scope incidents to the project
- **deployments:** Verify reused single-worker images and try spare builders
- **ai-gateway:** Honour self-hosted capability declarations in AI chat
- **ai-chat:** Pick and explain chat providers by declared capabilities
- **ai-chat:** Check tools for the saved model and pick defaults in one query
- **analytics:** Let keyed ingest beacons through CORS
- **sdk:** Send keyed beacons as text/plain so they skip the CORS preflight
- **analytics:** Cap failed key lookups and drop per-request copies on ingest
- **web:** Keep typed characters in URL-bound search inputs
- **web:** Confirm before deleting alert rules, notification providers and routes, and email providers
- **web:** Handle failed saves and sign-in without uncaught rejections
- **web:** Stack confirmation dialogs above sheets
- **web:** Only request compose-security for Compose projects
- **web:** Read vulnerability detail from the paginated response and show not-found
- **web:** Show not-found for unknown backup schedule and storage upgrade ids
- **web:** Carry the query string across legacy redirects
- **web:** Keep the webhook delete confirmation open
- **web:** Let numeric settings fields be emptied while editing
- **web:** Address review findings on URL state, vulnerability retry and dialog stacking
- **web:** Keep an emptied provider SMTP port and timeout empty
- **web:** Address review findings on redirects, OIDC priority, upgrade logs and test mocks
- **deps:** Replace unmaintained serde_yaml with yaml_serde ([#1457](https://github.com/gotempsh/temps/issues/1457))
- **status-page:** Close custom-domain races, renew certificates and release stored copies
- **web:** Refresh status page domains on publish changes and offer renewal
- **status-page:** Keep certificate calls on the binding's own order
- **status-page:** Address certificate rows by id instead of holding a lock
- **status-page:** Serialize certificate operations per hostname and store copies atomically
- **web:** AI gateway provider choice, stopped-deployment rollback, secret editing and accessibility ([#1459](https://github.com/gotempsh/temps/issues/1459))
- **web:** Find vulnerabilities beyond the first page of a scan ([#1461](https://github.com/gotempsh/temps/issues/1461))
- **providers:** Never replace a managed container without its data ([#1455](https://github.com/gotempsh/temps/issues/1455))
- **backend:** Resolve the 2026-10-12 QA backend batch ([#1466](https://github.com/gotempsh/temps/issues/1466))
- **web:** Frontend fixes for issues 1425-1453 ([#1463](https://github.com/gotempsh/temps/issues/1463))
- **domains:** Keep the old ACME order and DNS cleanup receipt until its replacement exists ([#1465](https://github.com/gotempsh/temps/issues/1465))
- **web:** Remaining console accessibility findings and a safer sandbox file save ([#1474](https://github.com/gotempsh/temps/issues/1474))

### Miscellaneous

- **sdk:** Release analytics-core v0.0.6, react-analytics v0.0.8, analytics-browser v0.0.7, svelte-analytics v0.0.5, vue-analytics v0.0.5

### Refactor

- **ai-gateway:** Move self-hosted model selection checks into the gateway service
- **web:** Validate the add-provider form with React Hook Form and Zod
- **web:** Read per-key settings save state from React Query

### Testing

- **backup:** Check backup uploads against a real Object Lock bucket
- **web:** Cover journey table header sorting and rename url-state test
- **web:** Cover emptied SMTP port and timeout through the provider form
- **web:** Keep the provider form test free of render-time assignments
- Fix three failures on main from #1455 and #1459 behaviour changes ([#1464](https://github.com/gotempsh/temps/issues/1464))

## [0.1.0-beta.57] - 2026-10-11

### Added

- **proxy:** Serve third-party callbacks on a public-only ingress surface
- **email:** Resend captured emails

### Documentation

- **security:** Use david@temps.sh as the vulnerability report address ([#1382](https://github.com/gotempsh/temps/issues/1382))
- **adr:** Add ADR-051 public ingress surface for third-party callbacks

### Fixed

- **web:** Show real load errors, stop doomed requests, and fix console a11y failures ([#1380](https://github.com/gotempsh/temps/issues/1380))
- **deployments:** Single route_generation writer, root directory checked on save, worker Vite build coverage ([#1381](https://github.com/gotempsh/temps/issues/1381))
- QA retest findings — S3 errors, Elixir umbrellas, screenshot hang, HA replica race, pg_dump temp dir, failover after CP outage ([#1395](https://github.com/gotempsh/temps/issues/1395))
- **compose:** Show why a Compose deployment failed, and pass project variables to builds ([#1397](https://github.com/gotempsh/temps/issues/1397))
- QA retest findings #1389–#1394 (DB startup wait, operation status, Drop ZIPs, node placement, HA standby protection, console a11y) ([#1398](https://github.com/gotempsh/temps/issues/1398))
- **web:** Keep the requested page when paginating service backups
- **web:** Keep the plugins tab in the URL
- **web:** Show honest email domain states for SMTP and project access
- **web:** Let dev-server requests pass same-origin guards
- **email:** Keep failed resends resendable and never show a paused SMTP domain as sending
- **email:** Refuse resending an email whose stored headers can no longer be sent
- **email:** Keep a resend's claim when its provider result could not be saved
- **email:** Fence resends so a stalled attempt cannot deliver or overwrite
- **email:** Return a rejected resend to captured when its result cannot be saved
- **deployments:** Pass build-argument values to worker builds for every preset
- **deployments:** Record only platform-set values as public build arguments
- **deployer:** Join continued Dockerfile lines the way Docker does
- **proxy:** Keep gate-denied requests off the MCP endpoint in dual-listener mode
- **config:** Refuse a public ingress hostname that any application uses
- **web:** Never show the console origin as a callback URL
- **config:** Keep the public ingress hostname out of the preview namespace
- **agents:** Skip the microsandbox SDK on Intel macOS so the release binary builds

### Miscellaneous

- **release:** @temps-sdk/cli 0.1.37 + analytics SDKs, repin skills ([#1396](https://github.com/gotempsh/temps/issues/1396))

## [0.1.0-nightly.20261010.47d472a3f] - 2026-10-10

### Added

- **agents:** Add experimental microsandbox sandbox backend ([#1323](https://github.com/gotempsh/temps/issues/1323))
- **nodes:** Dedicated nodes that only run environments pinning them ([#1341](https://github.com/gotempsh/temps/issues/1341))
- **security:** Let admins trust private networks for outbound requests ([#1363](https://github.com/gotempsh/temps/issues/1363))

### CI

- Skip Rust jobs and reuse main's binary when no Rust input changed ([#1361](https://github.com/gotempsh/temps/issues/1361))

### Documentation

- Correct CLI and API examples to match real commands and routes ([#1365](https://github.com/gotempsh/temps/issues/1365))

### Fixed

- **web:** Explain invalid ZIP uploads and allow replacement ([#1335](https://github.com/gotempsh/temps/issues/1335))
- **providers:** Reject PostgreSQL upgrades to the selected image ([#1340](https://github.com/gotempsh/temps/issues/1340))
- **web:** Make console setup and documentation links actionable ([#1336](https://github.com/gotempsh/temps/issues/1336))
- **drop:** Build Rsbuild output from uploaded source ([#1343](https://github.com/gotempsh/temps/issues/1343))
- **deployments:** Retain context for nested workspace apps ([#1342](https://github.com/gotempsh/temps/issues/1342))
- **web:** Honest read-failure states, slug-based telemetry setup link, and commit-pinned redeploys ([#1348](https://github.com/gotempsh/temps/issues/1348))
- **web:** Move the drop delivery provider into a compact summary row ([#1358](https://github.com/gotempsh/temps/issues/1358))
- **proxy:** Answer 503 for stopped apps instead of the console ([#1360](https://github.com/gotempsh/temps/issues/1360))
- **storage:** Keep RustFS starting until authenticated S3 readiness and explain failed initialization ([#1362](https://github.com/gotempsh/temps/issues/1362))
- Preserve path on domain redirects, HTTPS redirect behind proxies, unbiased web-vitals beacons and p75 API ([#1359](https://github.com/gotempsh/temps/issues/1359))
- **qa:** Restore static rollback and safe storage recovery ([#1364](https://github.com/gotempsh/temps/issues/1364))
- **deployer:** Keep BuildKit cache mounts per project on worker builds ([#1372](https://github.com/gotempsh/temps/issues/1372))
- **agent:** Resync routes within seconds after the control plane stops refusing a node ([#1374](https://github.com/gotempsh/temps/issues/1374))
- **providers:** Stop HA clusters by member and report pre-upgrade backup failures ([#1366](https://github.com/gotempsh/temps/issues/1366))
- **templates:** Pin the curated observability starter image by digest ([#1375](https://github.com/gotempsh/temps/issues/1375))
- **docs:** Drop the /howto/ segment from two guide links ([#1377](https://github.com/gotempsh/temps/issues/1377))
- **deployments:** Report take_screenshot as pending until a real image is captured ([#1376](https://github.com/gotempsh/temps/issues/1376))
- **deployments:** Move uploaded-source apps on drain, account for worker containers on node removal, build nested Go/Cargo apps ([#1367](https://github.com/gotempsh/temps/issues/1367))
- **deployments:** Base image retention on what the builder recorded ([#1378](https://github.com/gotempsh/temps/issues/1378))
- **providers:** Roll back cluster lock transactions when a locked check fails ([#1379](https://github.com/gotempsh/temps/issues/1379))

## [0.1.0-nightly.20261009.909788014] - 2026-10-09

### Added

- **providers:** Import data from external databases into managed services ([#1306](https://github.com/gotempsh/temps/issues/1306))
- **deployments:** Let projects and environments build on worker nodes ([#1321](https://github.com/gotempsh/temps/issues/1321))

### Fixed

- **migrations:** Lift TimescaleDB decompression cap for monitor reconciliation ([#1307](https://github.com/gotempsh/temps/issues/1307))
- **deps:** Upgrade next to 16.3.8 and multidict to 6.9.1 ([#1312](https://github.com/gotempsh/temps/issues/1312))
- Console honesty fixes for SMTP tests, backups card, health alerts, image pull failures, RustFS diagnostics ([#1313](https://github.com/gotempsh/temps/issues/1313))
- **drop:** Deploy Deno, Phoenix, plain PHP and Swift starters; qualify Dockerfile and Spring starters ([#1322](https://github.com/gotempsh/temps/issues/1322))
- **otel:** Bound every ClickHouse read and make facets discoverable ([#1325](https://github.com/gotempsh/temps/issues/1325))
- **providers:** Release the Redis database a failed import created ([#1324](https://github.com/gotempsh/temps/issues/1324))
- **security:** Close #1210 follow-ups for cluster app auth and sandbox git tokens ([#1333](https://github.com/gotempsh/temps/issues/1333))

### Testing

- **providers:** Restart legacy SCRAM cluster members like production ([#1305](https://github.com/gotempsh/temps/issues/1305))

## [0.1.0-nightly.20261008.dad133bdd] - 2026-10-08

### Added

- **deployments:** Deploy static sites built on worker nodes ([#1299](https://github.com/gotempsh/temps/issues/1299))

### Fixed

- **providers:** Make control-plane HA members reachable from worker nodes ([#1294](https://github.com/gotempsh/temps/issues/1294))
- **git:** Refresh and check new GitHub App installations ([#1296](https://github.com/gotempsh/temps/issues/1296))
- Close v0.1.0 GA milestone issues ([#1295](https://github.com/gotempsh/temps/issues/1295))
- **otel:** Read global traces page-first from local ClickHouse ([#1297](https://github.com/gotempsh/temps/issues/1297))
- **web:** Preserve AI gateway setup errors and retry context ([#1298](https://github.com/gotempsh/temps/issues/1298))

## [0.1.0-nightly.20261007.2c1f296bc] - 2026-10-07

### CI

- Force incremental builds in the musl cache pruning test ([#1278](https://github.com/gotempsh/temps/issues/1278))

### Fixed

- **release:** Integrate v0.1.0 hardening and qualification gates ([#1274](https://github.com/gotempsh/temps/issues/1274))
- **release:** Keep waiting when the runs API drops a queued test run ([#1275](https://github.com/gotempsh/temps/issues/1275))
- **restore:** Stream restore inputs, scope dump files per attempt, document restore 409s ([#1276](https://github.com/gotempsh/temps/issues/1276))
- **web:** Render Git connection selector with standard tabs ([#1277](https://github.com/gotempsh/temps/issues/1277))
- **web:** Repair console workflows for navigation, settings, backups, alerting and failures ([#1291](https://github.com/gotempsh/temps/issues/1291))
- **upgrade:** Make the pre-migration backup opt-in and reap orphaned dumps ([#1286](https://github.com/gotempsh/temps/issues/1286))
- Resolve open v0.1.0 issues across proxy, cron, alerting, presets and console ([#1293](https://github.com/gotempsh/temps/issues/1293))

## [0.1.0-nightly.20261006.e70c36a7] - 2026-10-06

### Added

- **cloud-protocol:** Console-proxy capability and frame family (ADR-045)
- **cloud-client:** In-process console-proxy worker (ADR-045 WP-B)
- **cloud:** Console-access switch, managed Cloud OIDC provider and role gate (ADR-045 WP-C)
- **cloud:** Start the console-proxy worker from the Cloud plugin (ADR-045)
- **web,cli:** Console-access switch, Cloud login copy and managed-provider guards (ADR-045)
- **upgrade:** Back up migrations, guard schemas and test rollback ([#1227](https://github.com/gotempsh/temps/issues/1227))
- **settings:** Disclose and control anonymous telemetry at runtime ([#1229](https://github.com/gotempsh/temps/issues/1229))
- **deployments:** Explain failures and configure readiness deadlines ([#1228](https://github.com/gotempsh/temps/issues/1228))
- **ai-gateway:** Support OpenAI Responses and batch workflows ([#1242](https://github.com/gotempsh/temps/issues/1242))

### CI

- Enforce clippy and run frontend quality checks ([#1230](https://github.com/gotempsh/temps/issues/1230))
- **first-run:** Nightly first-run suite and quiet-logs soak ([#1218](https://github.com/gotempsh/temps/issues/1218))

### Documentation

- **adr:** 045 console access through Cloud
- **cloud:** Align generated consent guidance with enrollment
- **tutorials:** Sync the deploy guides with the published site ([#1244](https://github.com/gotempsh/temps/issues/1244))
- **tutorials:** Note Rails static files and Django collectstatic in recent builds ([#1267](https://github.com/gotempsh/temps/issues/1267))

### Fixed

- **cloud:** Fence console access and preserve feature consent
- **auth:** Fence OIDC MFA sessions against provider revocation
- **auth:** Bind OIDC challenges and sessions to their issuing provider
- **cloud:** Honor console consent during provisioning and wake relay promptly
- **cloud:** Preserve relay backoff after short connections
- **cloud:** Protect relay headers and keep cancellation responsive
- **deployments:** Preserve usable Git metadata in Docker builds ([#1223](https://github.com/gotempsh/temps/issues/1223))
- **monitoring:** Avoid false alarms on fresh installations ([#1225](https://github.com/gotempsh/temps/issues/1225))
- **onboarding:** Clarify service changes, charts and unavailable commands ([#1232](https://github.com/gotempsh/temps/issues/1232))
- **providers:** Require SCRAM authentication for HA Postgres infrastructure ([#1233](https://github.com/gotempsh/temps/issues/1233))
- **builds:** Upgrade Autopack to v0.1.4
- **deployments:** Protect other instances during container cleanup ([#1234](https://github.com/gotempsh/temps/issues/1234))
- **security:** Bind restores, limit credentials and parameterize metrics ([#1231](https://github.com/gotempsh/temps/issues/1231))
- **drop:** Detect Ruby, PHP and TanStack Start server applications ([#1263](https://github.com/gotempsh/temps/issues/1263))
- **observability:** Bound recurring failure logs and report recovery ([#1235](https://github.com/gotempsh/temps/issues/1235))
- **release:** Gate publishing, separate channels, and verify upgrades ([#1221](https://github.com/gotempsh/temps/issues/1221)) [**BREAKING**]
- **deps:** Drop vulnerable opentelemetry_sdk 0.30 and repair the web ESLint job ([#1265](https://github.com/gotempsh/temps/issues/1265))
- **presets:** Pass project variables to Autopack build steps ([#1266](https://github.com/gotempsh/temps/issues/1266))
- **notifications:** Show the project slug instead of raw IDs in Slack alerts ([#1268](https://github.com/gotempsh/temps/issues/1268))
- **nodes:** Harden the WireGuard mesh, node pairing and SSH enrollment ([#1260](https://github.com/gotempsh/temps/issues/1260))
- **restore:** Recover interrupted restores and explain restore and backup failures ([#1273](https://github.com/gotempsh/temps/issues/1273))

### Miscellaneous

- **deps-dev:** Bump typescript from 6.0.2 to 7.0.2 in /web ([#1247](https://github.com/gotempsh/temps/issues/1247))

### Performance

- **builds:** Accelerate Next.js builds and report BuildKit timings ([#1226](https://github.com/gotempsh/temps/issues/1226))

## [0.1.0-nightly.20261005.5f4906f4] - 2026-10-05

### Added

- **nodes:** Add managed WireGuard mesh and SSH enrollment ([#1186](https://github.com/gotempsh/temps/issues/1186))
- **ai:** Simplify harness setup and connect OpenCode to OpenAI-compatible APIs ([#1187](https://github.com/gotempsh/temps/issues/1187))
- **web:** One-click first deploy and a clearer Quick Start for new installs ([#1220](https://github.com/gotempsh/temps/issues/1220))

### Documentation

- Fill GA documentation gaps and tidy repository hygiene ([#1219](https://github.com/gotempsh/temps/issues/1219))

### Fixed

- **security:** Patch Dependabot alerts and Trivy container-scan findings ([#1191](https://github.com/gotempsh/temps/issues/1191))
- First-run blockers — Flexible image deploys, docker context, job-log 500s and log noise ([#1192](https://github.com/gotempsh/temps/issues/1192))
- **console:** Explain startup failures without interrupting apps ([#1224](https://github.com/gotempsh/temps/issues/1224))
- **deployments:** Always show build logs, with HTTP fallback and dev WebSocket proxy ([#1217](https://github.com/gotempsh/temps/issues/1217))

## [0.1.0-nightly.20261004.32a8e9c1] - 2026-10-04

### Added

- **dns:** Ownership-guarded managed DNS records + Cloudflare proxied mode (ADR-031) ([#347](https://github.com/gotempsh/temps/issues/347))
- **secrets:** Add credential checks, local expiry checks and history to secrets ([#1188](https://github.com/gotempsh/temps/issues/1188))

### CI

- Skip build and test jobs on documentation-only pull requests ([#1182](https://github.com/gotempsh/temps/issues/1182))

### Documentation

- **upgrade:** Move old Compose stacks' PostgreSQL data into the volume ([#1180](https://github.com/gotempsh/temps/issues/1180))

### Fixed

- **proxy:** Serve a static site's own 404.html instead of the SPA shell ([#1184](https://github.com/gotempsh/temps/issues/1184))
- **nodes:** Stop node removal deadlocking with the DNS publisher ([#1185](https://github.com/gotempsh/temps/issues/1185))

### Testing

- **e2e:** Multi-node over WireGuard between isolated networks, and fix the worker overlay MTU ([#1189](https://github.com/gotempsh/temps/issues/1189))
- **web:** Include local_artifacts in the mocked env var detection response ([#1190](https://github.com/gotempsh/temps/issues/1190))

## [0.1.0-nightly.20261003.6f74680e] - 2026-10-02

### Added

- **sandbox:** Run sandboxes on worker nodes (ADR-048) ([#1173](https://github.com/gotempsh/temps/issues/1173))

### Fixed

- Bugs reported from a production fork (error grouping, CLI login, OTLP listener, retention, SDK) ([#1175](https://github.com/gotempsh/temps/issues/1175))

## [0.1.0-nightly.20261002.2d6c7780] - 2026-10-02

### Added

- Simplify onboarding and support External projects ([#1153](https://github.com/gotempsh/temps/issues/1153))

### Fixed

- Patch web audit advisories and Rust 1.99 clippy lints ([#1176](https://github.com/gotempsh/temps/issues/1176))
- **presets:** Honor pnpm configuration and detect pnpm in React App ([#1177](https://github.com/gotempsh/temps/issues/1177))
- **presets:** Do not follow symlinks when scanning a repository ([#1178](https://github.com/gotempsh/temps/issues/1178))

## [0.1.0-nightly.20260930.53d4fb40] - 2026-09-29

### Documentation

- **request-flow:** Describe the proxy pipeline as implemented ([#1170](https://github.com/gotempsh/temps/issues/1170))

### Fixed

- **cli:** Exit non-zero when resource flags fail validation ([#1168](https://github.com/gotempsh/temps/issues/1168))
- **deployments:** Set PORT to the port detected from the image ([#1169](https://github.com/gotempsh/temps/issues/1169))
- **config:** Return on_demand_tls from GET /settings and pin a lossless settings round-trip ([#1172](https://github.com/gotempsh/temps/issues/1172))

## [0.1.0-nightly.20260929.95fdac25] - 2026-09-29

### Documentation

- **multi-node:** Describe least-loaded scheduling and current limitations ([#1157](https://github.com/gotempsh/temps/issues/1157))

### Fixed

- **cli:** Offer stopped deployments in the rollback picker ([#1163](https://github.com/gotempsh/temps/issues/1163))
- **cli:** Convert CPU flags to the API's microcore units ([#1162](https://github.com/gotempsh/temps/issues/1162))

## [0.1.0-nightly.20260928.f6f763e0] - 2026-09-28

### Added

- **compose:** Allow several public URLs per Compose service ([#1152](https://github.com/gotempsh/temps/issues/1152))

### Fixed

- Network creation race, import DB URLs and Coolify values, Visit domain, previews-off deploys ([#1154](https://github.com/gotempsh/temps/issues/1154))

## [0.1.0-nightly.20260927.d30d578d] - 2026-09-27

### Added

- **telemetry:** Report operation outcomes and failures ([#1148](https://github.com/gotempsh/temps/issues/1148))

### Fixed

- **proxy:** Join HTTP/2 cookie crumbs into one Cookie header upstream ([#1149](https://github.com/gotempsh/temps/issues/1149))
- **email:** Verify Scaleway domains against the DKIM record Scaleway checks ([#1150](https://github.com/gotempsh/temps/issues/1150))

### Testing

- **web:** Give Cloud telemetry e2e fixtures the delivery fields ([#1151](https://github.com/gotempsh/temps/issues/1151))

## [0.1.0-nightly.20260926.ed69c4e6] - 2026-09-26

### Added

- **deployments:** Build source images on worker nodes ([#1146](https://github.com/gotempsh/temps/issues/1146))

### Documentation

- **rollbacks,scaling:** Fix fabricated CLI commands, wrong defaults, and a CLI unit bug ([#1130](https://github.com/gotempsh/temps/issues/1130))

### Fixed

- **telemetry:** Use a random persisted telemetry ID for stateless control planes ([#1145](https://github.com/gotempsh/temps/issues/1145))
- **otel:** Alert on Cloud delivery failures only while they are happening ([#1147](https://github.com/gotempsh/temps/issues/1147))

## [0.1.0-nightly.20260926.de626de6] - 2026-09-26

### CI

- Stop the Actions cache from growing past what builds use ([#1142](https://github.com/gotempsh/temps/issues/1142))

### Fixed

- **ci:** Replace withdrawn MinIO images with RustFS and the rc client ([#1137](https://github.com/gotempsh/temps/issues/1137))
- **ui:** Compact env and log views with rolling time ranges ([#1140](https://github.com/gotempsh/temps/issues/1140))
- **deployments:** Stream uploaded images to remote nodes instead of pulling ([#1139](https://github.com/gotempsh/temps/issues/1139))
- **logs:** Replay every WAL generation the writer produces and never pause collection on one it cannot ([#1132](https://github.com/gotempsh/temps/issues/1132))
- **monitoring:** Stop crash alarms for superseded deployments' containers and add bulk alarm actions ([#1138](https://github.com/gotempsh/temps/issues/1138))
- **telemetry-api:** Restore country geolocation and backfill unknown countries ([#1144](https://github.com/gotempsh/temps/issues/1144))

### Testing

- **e2e:** Align console specs with rolling time presets and compose filters ([#1143](https://github.com/gotempsh/temps/issues/1143))

## [0.1.0-nightly.20260925.9edf44ab] - 2026-09-25

### Added

- **git:** Git connection detail page with paginated repositories ([#1134](https://github.com/gotempsh/temps/issues/1134))
- **auth:** Let admins reset another user's password ([#1078](https://github.com/gotempsh/temps/issues/1078)) ([#1135](https://github.com/gotempsh/temps/issues/1135))

### Fixed

- **config:** Preserve cluster CA during settings saves ([#1115](https://github.com/gotempsh/temps/issues/1115))
- **monitoring:** Reconcile worker ports safely ([#1118](https://github.com/gotempsh/temps/issues/1118))
- **deployments:** Support remote promote and rollback ([#1122](https://github.com/gotempsh/temps/issues/1122))
- **status-page:** Probe managed monitors through local proxy ([#1119](https://github.com/gotempsh/temps/issues/1119))
- **git:** Authenticate public repository deployments ([#1128](https://github.com/gotempsh/temps/issues/1128))
- **database:** Log SQL statements at TRACE, keep slow-query warnings ([#1129](https://github.com/gotempsh/temps/issues/1129))
- **logs:** Collect only owned containers, name facets across the range; size provider logo ([#1133](https://github.com/gotempsh/temps/issues/1133))
- **proxy:** Stop serving gzip bytes as text/markdown ([#1136](https://github.com/gotempsh/temps/issues/1136))

## [0.1.0-nightly.20260924.68c44d50] - 2026-09-24

### Documentation

- **admin-listener:** Move AI gateway to the admin route surface ([#1092](https://github.com/gotempsh/temps/issues/1092))
- **security:** Remove fabricated content, fix attack-mode and log retention claims ([#1109](https://github.com/gotempsh/temps/issues/1109))

### Fixed

- **proxy:** Route project wildcard domains and wake sleeping environments ([#1103](https://github.com/gotempsh/temps/issues/1103))
- **console:** Hide sparse checkout option at repository root ([#1102](https://github.com/gotempsh/temps/issues/1102))
- **compose:** Make policy checks individually actionable ([#1093](https://github.com/gotempsh/temps/issues/1093))
- **startup:** Bound WAL recovery and decouple proxy from console initialization ([#1104](https://github.com/gotempsh/temps/issues/1104))
- **backup:** Use engine-specific exact WAL-G retention deletion ([#1105](https://github.com/gotempsh/temps/issues/1105))
- **compose:** Return complete preview problem details ([#1106](https://github.com/gotempsh/temps/issues/1106))
- **backup:** Type WAL-G deletion identity ([#1107](https://github.com/gotempsh/temps/issues/1107))
- **dns:** Keep cluster DNS available without the console ([#1110](https://github.com/gotempsh/temps/issues/1110))
- **deployer:** Report safe Compose config diagnostics ([#1112](https://github.com/gotempsh/temps/issues/1112))
- **cloud:** Expose disconnect recovery for rejected credentials ([#1111](https://github.com/gotempsh/temps/issues/1111))
- **logs:** Recover WAL without blocking console startup ([#1108](https://github.com/gotempsh/temps/issues/1108))
- **docker:** Run release binaries in a compatible glibc runtime ([#1113](https://github.com/gotempsh/temps/issues/1113))
- **cli:** Fail static deployment waits on permanent status errors ([#1114](https://github.com/gotempsh/temps/issues/1114))
- **env:** Make preview variable scope explicit and inherited ([#1116](https://github.com/gotempsh/temps/issues/1116))

### Testing

- **e2e:** Wait for asynchronously created default monitor ([#1094](https://github.com/gotempsh/temps/issues/1094))

## [0.1.0-nightly.20260923.a7bfeb68] - 2026-09-23

### Fixed

- **compose:** Resolve remote references and reset policies safely ([#1090](https://github.com/gotempsh/temps/issues/1090))
- **logs:** Make global and runtime logs readable on mobile ([#1091](https://github.com/gotempsh/temps/issues/1091))

## [0.1.0-nightly.20260923.9b9e5b45] - 2026-09-23

### Added

- **control-plane:** Support replaceable stateless installations ([#1081](https://github.com/gotempsh/temps/issues/1081))
- **git:** Optionally clone only the project root directory ([#1067](https://github.com/gotempsh/temps/issues/1067))
- **cloud:** Reconnect installations to an existing cloud identity ([#1088](https://github.com/gotempsh/temps/issues/1088))

### Fixed

- **backup:** Report WAL-G per-backup size ([#1086](https://github.com/gotempsh/temps/issues/1086))
- **deployer:** Preflight Docker Compose availability ([#1085](https://github.com/gotempsh/temps/issues/1085))
- **logs:** Purge unsealed head-buffer lines, not just sealed chunks ([#1082](https://github.com/gotempsh/temps/issues/1082))
- **logs:** Preserve legacy chunks during v2 migration ([#1087](https://github.com/gotempsh/temps/issues/1087))
- **git:** Terminate sparse clone helpers on cancellation ([#1089](https://github.com/gotempsh/temps/issues/1089))

## [0.1.0-nightly.20260922.c728ab64] - 2026-09-22

### Added

- **monitoring:** Add automatic credential checks and variable history ([#1072](https://github.com/gotempsh/temps/issues/1072))
- **analytics:** Let deployment tokens enrich their own project's visitors ([#1076](https://github.com/gotempsh/temps/issues/1076))
- **ingress:** Serve public app traffic through workers ([#1077](https://github.com/gotempsh/temps/issues/1077))
- **logs:** Object-store log chunks with ClickHouse line index (ADR-046/047) ([#1064](https://github.com/gotempsh/temps/issues/1064))
- **deployer:** Host Docker socket grant for operator-owned infrastructure services (ADR 045) ([#1068](https://github.com/gotempsh/temps/issues/1068))

### Fixed

- **ci:** Retire the stale nextest target seed that breaks the docker-tests archive ([#1079](https://github.com/gotempsh/temps/issues/1079))
- **deployments:** Keep failure reports within the central endpoint's size limit ([#1080](https://github.com/gotempsh/temps/issues/1080))

### Miscellaneous

- **deps-dev:** Bump typescript from 6.0.2 to 7.0.2 in /web ([#1070](https://github.com/gotempsh/temps/issues/1070))

## [0.1.0-nightly.20260921.3bd97125] - 2026-09-20

### Added

- **cloud:** TEMPS_CLOUD_BACKEND_URL one-shot bootstrap for unattended enrollment ([#1061](https://github.com/gotempsh/temps/issues/1061))
- **auth:** Cloud-managed OIDC provider with instance role gate ([#1055](https://github.com/gotempsh/temps/issues/1055))
- **auth:** Cloud console-access OIDC bootstrap file + fixed managed-login start ([#1063](https://github.com/gotempsh/temps/issues/1063))
- **compose:** Add per-project security policy exceptions ([#1057](https://github.com/gotempsh/temps/issues/1057))

### Fixed

- **ui:** Restore log counts sidebar and use provider logos ([#1056](https://github.com/gotempsh/temps/issues/1056))
- **otel:** Bound Cloud trace reads and restore AI activity ([#1058](https://github.com/gotempsh/temps/issues/1058))
- **serve:** Let --profile control-plane boot without a Docker socket ([#1062](https://github.com/gotempsh/temps/issues/1062))
- **control-plane:** Run without Docker, ask for a worker node instead of failing ([#1059](https://github.com/gotempsh/temps/issues/1059))
- **backup:** Fail a backup this process cannot run instead of parking it ([#1066](https://github.com/gotempsh/temps/issues/1066))

### Testing

- **web:** Fix 9 failing e2e specs on main ([#1065](https://github.com/gotempsh/temps/issues/1065))

## [0.1.0-nightly.20260920.760cda68] - 2026-09-19

### Added

- **plugins:** Install from subdirectories and select update refs ([#1045](https://github.com/gotempsh/temps/issues/1045))
- Unattended Cloud enrollment env var + source-free release compose ([#1044](https://github.com/gotempsh/temps/issues/1044))
- **analytics:** Add guided AI visitor activity reports ([#1039](https://github.com/gotempsh/temps/issues/1039))
- **plugins:** Improve install review and show live progress ([#1049](https://github.com/gotempsh/temps/issues/1049))
- **ui:** Polish settings, observability, and domain management ([#1050](https://github.com/gotempsh/temps/issues/1050))
- **plugins:** Simplify running rows and standardize embedded workspaces ([#1054](https://github.com/gotempsh/temps/issues/1054))

### Fixed

- **web:** Align time-range tests and remove nested settings gutter ([#1047](https://github.com/gotempsh/temps/issues/1047))
- **env:** Keep marked secrets write-only ([#1046](https://github.com/gotempsh/temps/issues/1046))
- **web:** Restore navigation and plugin E2E coverage ([#1051](https://github.com/gotempsh/temps/issues/1051))
- **nodes:** Wait out a grace period before failing over an offline node ([#1053](https://github.com/gotempsh/temps/issues/1053))

## [0.1.0-nightly.20260919.317c141e] - 2026-09-19

### Added

- **error-tracking:** Resolve tunneled Sentry envelopes from the DSN; Retry-After on keyed ingest
- **serve:** Control-plane profile that runs no local workloads ([#1031](https://github.com/gotempsh/temps/issues/1031))
- **geo:** Keep MaxMind GeoLite2 database and IP lookups fresh ([#1036](https://github.com/gotempsh/temps/issues/1036))
- **cli:** Add local plugin runner with fixtures and event testing ([#1035](https://github.com/gotempsh/temps/issues/1035))
- **logs:** Archive finished build/deploy logs to S3-compatible storage ([#1040](https://github.com/gotempsh/temps/issues/1040))
- **deployer,file-store,proxy:** S3-compatible static-site and CAS storage with a hot-path byte cache ([#1041](https://github.com/gotempsh/temps/issues/1041))
- **ds:** Codify console design system and migrate reference pages ([#1037](https://github.com/gotempsh/temps/issues/1037))
- **cloud-protocol,cloud-client:** Instance status reporting (ADR-039) ([#1042](https://github.com/gotempsh/temps/issues/1042))

### Documentation

- **skills:** Guide plugin authors through design, testing and publishing ([#1038](https://github.com/gotempsh/temps/issues/1038))

### Fixed

- **scheduling:** Due-based monitors, persisted alert breach state, due-only cron scans
- **auth:** Correct middleware layer order in auth rate limiter ([#1043](https://github.com/gotempsh/temps/issues/1043))

## [0.1.0-nightly.20260918.e006bb00] - 2026-09-17

### Added

- **plugins:** Add host-brokered AI with persistent actors and grants ([#1029](https://github.com/gotempsh/temps/issues/1029))

### Fixed

- **deployments:** Update stale MockDatabase fixture in multinode integration test ([#1028](https://github.com/gotempsh/temps/issues/1028))
- **web:** Resolve React lint errors and warnings ([#1027](https://github.com/gotempsh/temps/issues/1027))
- **monitoring:** Show names/slugs instead of raw numeric IDs in alert notifications ([#1030](https://github.com/gotempsh/temps/issues/1030))

## [0.1.0-nightly.20260917.b823a663] - 2026-09-17

### Documentation

- **sdks:** Document externally hosted SPA ingest topology (ADR-040) ([#1023](https://github.com/gotempsh/temps/issues/1023))

### Fixed

- **deployments:** Redeploy images by deployment source ([#1016](https://github.com/gotempsh/temps/issues/1016))
- **ai:** Diagnose unreachable credential relays ([#1018](https://github.com/gotempsh/temps/issues/1018))
- **preview:** Restore sandbox routes after gateway restart ([#1022](https://github.com/gotempsh/temps/issues/1022))
- **analytics:** Count identified visitors, not sessions, for active-visitors badge ([#1024](https://github.com/gotempsh/temps/issues/1024))
- **deployments:** Prevent failover redeploy storms ([#1019](https://github.com/gotempsh/temps/issues/1019))
- **deployer:** Detect out-of-memory build steps and report build caps honestly ([#1025](https://github.com/gotempsh/temps/issues/1025))

### Performance

- **observability:** Speed traces and continue log searches ([#1017](https://github.com/gotempsh/temps/issues/1017))

## [0.1.0-nightly.20260916.33eb6be3] - 2026-09-16

### Added

- **proxy:** Add request authorization extension hook ([#1007](https://github.com/gotempsh/temps/issues/1007))
- **cli:** Add SSH setup proof of concept ([#1005](https://github.com/gotempsh/temps/issues/1005))
- **ai:** Add harness preflight checks and consented smoke tests ([#1012](https://github.com/gotempsh/temps/issues/1012))
- **backup:** Record and show until when each backup is retained ([#1011](https://github.com/gotempsh/temps/issues/1011))
- **console:** Let extensions add links to the Settings sidebar ([#1015](https://github.com/gotempsh/temps/issues/1015))

### Fixed

- **plugins:** Restore "verified" wording dropped in repository-install refactor ([#1009](https://github.com/gotempsh/temps/issues/1009))
- **proxy:** Sanitize the password-wall redirect and rate-limit password guesses ([#1006](https://github.com/gotempsh/temps/issues/1006))
- **deployments:** Clean up legacy failed containers ([#1008](https://github.com/gotempsh/temps/issues/1008))
- **ai:** Report actionable credential verification failures ([#1010](https://github.com/gotempsh/temps/issues/1010))
- **ai:** Preserve subscription relay protocol headers ([#1014](https://github.com/gotempsh/temps/issues/1014))
- **cli:** Make local image uploads resilient ([#1013](https://github.com/gotempsh/temps/issues/1013))

## [0.1.0-nightly.20260915.03f32acc] - 2026-09-15

### Fixed

- **ci:** Populate shared daemon image build caches ([#1004](https://github.com/gotempsh/temps/issues/1004))

## [0.1.0-nightly.20260914.88203997] - 2026-09-14

### Added

- **proxy:** Allow admins to control loopback forwarded IP trust
- **config:** Audit loopback forwarded-IP trust changes
- **cli:** Automate TypeScript plugin publishing ([#978](https://github.com/gotempsh/temps/issues/978))

### Documentation

- **config:** Drop stale CLI/env override mentions from settings model

### Fixed

- **workspaces:** Gate releases and guide harness setup
- **logs:** Render ansi colors and remove page volume chart
- **workspaces:** Verify credentials and recover managed runtimes
- **workspaces:** Verify saved logins and preserve harness sessions
- **workspaces:** Verify both image architectures and type mcp
- **config:** Preserve credentials across concurrent settings saves
- **proxy:** Preserve forwarded client IPs from loopback proxies
- **proxy:** Select the final forwarded header without iteration
- **proxy:** Make local forwarded IP trust opt-in
- **proxy:** Allow forwarded IP trust through environment
- **proxy:** Retain forwarded IP trust on settings read failure
- **proxy:** Release refresh snapshot before polling sleep
- **proxy:** Load forwarded IP trust before serving
- **proxy:** Honor startup trust override without settings read
- **proxy:** Remove env/CLI override for loopback forwarded-IP trust
- **proxy:** Recognize IPv4-mapped IPv6 loopback peers
- **console:** Restore responsive deployment list items ([#983](https://github.com/gotempsh/temps/issues/983))
- **cli:** Drop the api.temps.sh relay default from temps join ([#984](https://github.com/gotempsh/temps/issues/984))
- **environments:** Stop gating plaintext env-var reveal behind SecretsRead ([#1002](https://github.com/gotempsh/temps/issues/1002))
- **web:** Expose AI crawler time ranges and scoped counts ([#1000](https://github.com/gotempsh/temps/issues/1000))
- **web:** Remove baseUrl for TypeScript 7 compatibility
- **web:** Type-check TypeScript 7 upgrade
- **release:** Pin runtime images before building release binaries ([#1003](https://github.com/gotempsh/temps/issues/1003))

### Miscellaneous

- **deps-dev:** Bump typescript from 5.9.2 to 7.0.2 in /web

### Testing

- **proxy:** Align mock listener fixture with main

## [0.1.0-nightly.20260914.ec1f1ee1] - 2026-09-14

### Added

- **console:** Add sortable analytics table columns

### Documentation

- Clarify Rust and frontend test placement

### Fixed

- **web:** Restore environment variables in project menu
- **ci:** Separate Trivy SARIF reporting from severity gate
- **deps:** Patch Next.js example and vulnerable transitive packages
- **ci:** Pin container scan image digest across Trivy passes
- **plugins:** Filter catalog by server platform
- **console:** Simplify deployment list structure
- **console:** Sort error groups by displayed range counts
- **backup:** Close the control-plane dump's schema-only tables over foreign keys
- **backup:** Fail the control-plane backup when the FK closure cannot be resolved
- **console:** Use full width page and form layouts
- **console:** Avoid duplicate settings gutters and test form panels
- **analytics:** Upsert visitor on session-replay init lookup miss
- **analytics:** Cap visitor creation from session-replay init
- **analytics:** Bound visitor id length on session-replay init upsert
- **analytics:** Address Greptile findings on session-replay visitor upsert
- **analytics:** Per-IP visitor-creation sub-limit, validate at entry
- **analytics:** Fix visitor-creation limiter check order and bound its IP map
- **analytics:** Shard visitor-creation IP limiter with DashMap

### Testing

- **console:** Verify backup form page widths
- **analytics:** Use generic referrer fixture in backfill test

## [0.1.0-nightly.20260913.5c9d477f] - 2026-09-13

### Added

- **sandbox:** Run commands through resident runtime daemon
- **cloud:** Create the nightly backup schedule for the managed destination
- **cloud:** Name the services whose archive is pinned elsewhere, default MariaDB schedules to Cloud
- **cloud:** Declare large in-place snapshots by manifest object
- **workspaces:** Integrate persistent runtimes and image releases
- **plugins:** Install signed registry binaries
- **plugins:** Add rotating registry trust chain

### CI

- Never restore web/node_modules from another lockfile's cache

### Fixed

- **web:** Reference console-kit as a workspace dependency, not file:
- **web,ci:** Drop the stale js-yaml devDependency, install from an exact cache key
- **cloud:** Release schedules on disconnect, never guess retention, keep the backup plugin optional
- **cloud:** Disable schedules atomically before deleting, document ensure responses
- **providers:** Switch MinIO images from Docker Hub to quay.io
- **providers:** Pin remaining test-fixture MinIO image to quay.io
- **providers:** Normalize persisted MinIO images to quay.io on load
- **e2e:** Accept singular detected preset
- **e2e:** Seed console project and update analytics selectors
- **logs:** Retain partial results when search reaches its limit
- **cloud:** Scope archive conflicts to the schedule's targets, keep existing MariaDB pins
- **backup:** Resume multipart uploads instead of aborting on a failed part
- **cloud:** Bound manifest reads, list sentinels by delimiter, keep typed upload errors
- **e2e:** Align observability specs with current console state
- **web:** Give global project scope picker an accessible name
- **traces:** Restore cross-project waterfall entry point
- **plugins:** Close registry review findings
- **plugins:** Close registry merge blockers
- **plugins:** Gate management controls by role
- **web:** Unify React types and patch js-yaml
- **plugins:** Refresh registry trust on demand
- **web:** Enforce a single React runtime
- **plugins:** Select artifacts by Linux libc
- **plugins:** Persist revocations and surface lifecycle failures

### Miscellaneous

- **sandbox:** Make runtime verification scripts executable

### Testing

- **e2e:** Assert selected project name inside picker
- **e2e:** Target scoped project picker after reload
- **plugins:** Prove candidate process transitions
- **plugins:** Exercise protocol v2 install flow

## [0.1.0-nightly.20260912.c0b03c64] - 2026-09-11

### Added

- **metrics:** Control-plane host I/O counters, latest-sample and docker disk usage endpoints
- **cli:** Server status, metrics and docker-disk-usage commands
- **web:** Control-plane Server page on the operator design system
- **console:** Unify navigation, observability, and database linking
- **cloud:** Declare managed backups by size, WAL-G repositories in place, report deletions

### Fixed

- **captcha-wasm:** Rebuild artifact for CI's Linux amd64 toolchain
- **console:** Unify layouts and polish settings workflows
- **metrics:** Count only hardware NICs and whole disks; URL range, pause and synced cursor on the Server page
- **deployments:** Measure the local Docker daemon, no DOCKER_HOST; page-level retry covers every metric query
- **cloud:** Re-declare a hashed copy when Cloud ignores in_place_root

### Miscellaneous

- **console:** Add required source attribution headers

### Refactor

- **web:** Rebuild the Server page in the current console design

### Testing

- **backup:** Cover retention and refused-queue BackupDeleted publication

## [0.1.0-nightly.20260911.194e154f] - 2026-09-11

### Added

- **cloud:** Catalog managed-source native snapshots in place

### Fixed

- **secrets:** Allow matching keys in separate environments
- **secrets:** Serialize environment deletion with scoped writes
- **monitors:** Edit check paths and sync compose health routes
- **backup:** Fail s3_mirror on Access Denied during comparison listing
- **backup:** Detect Access Denied streaming, not from a bounded tail
- **backup:** Enumerate RustFS buckets instead of whole-account mc mirror
- **backup:** Paginate bucket enumeration and fix per-bucket size prefix
- **cloud:** Resume interrupted backup mirror passes instead of restarting them
- **backup:** Keep the stderr watch verdict when the log drain times out
- **backup:** Fail closed when a watched stderr stream cannot be fully inspected
- **backup:** Treat any uninspected stderr as a failed watch, not only a slow drain
- **backup:** Name the backup and service in the s3_mirror access-denied error
- **captcha-wasm:** Reproduce committed package on Linux amd64
- **ci:** Stop blocking builds on captcha WASM byte differences
- **cloud:** Fall back to the copy path when Cloud rejects an in-place root
- **cloud:** Keep the copy path for WAL-G stream snapshots and type the stub DTOs

## [0.1.0-nightly.20260910.8322f2dc] - 2026-09-09

### Added

- **cloud-protocol:** Report the source database's actual Docker image
- **observability:** Add global logs traces errors and analytics endpoints

### Fixed

- **captcha-wasm:** Resync committed pkg/ with pinned-toolchain rebuild
- **compose:** Stop injecting a default CPU limit
- **web:** Map catalog-only preset slugs before requesting managed env vars
- **cloud:** Use a static human-readable source label for control-plane backups
- **deployer:** Grant baseline capabilities to every sandboxed Compose service
- **observability:** Disclose trace cutovers and filter hosting-provider speed samples
- **git:** Report the repository's actual default branch, not "main"
- **git:** Fetch default branch live instead of trusting the DB cache
- **git:** Keep branch listing resilient and sync remaining is_default clients
- **git:** Self-heal cached default branch and fix TS narrowing regression
- **web:** Let live is_default win over a stale defaultBranch prop
- **git:** Make default-branch persistence a single atomic UPDATE
- **git:** Don't gate the branch-cache fast path on a live provider call

### Testing

- **web:** Colocate provided-environment-variables tests with their module

## [0.1.0-nightly.20260909.b31a7b64] - 2026-09-09

### Added

- **ai:** Refresh provider model catalogs on demand

### Fixed

- **cloud:** Wake the backup mirror sweep on local backup completion
- **status-page:** Adopt existing environment monitors
- **migrations:** Preserve newer monitor paths on rollback
- **migrations:** Distinguish monitor path changes
- **status-page:** Track monitor path revisions
- **cloud:** Mirror control-plane backups instead of rejecting them as non-WAL-G
- **agent:** Bind published container ports to the node's private address, not 0.0.0.0
- **agent:** Reject dangerous private_address overrides (0.0.0.0, loopback, etc.)
- **agent:** Normalize private_address override to a bare IP before binding
- **deployments:** Stop truncating bare IPv6 private_address values
- **deployments,cli:** Normalize private_address end-to-end, not just at agent startup
- **routes,cli:** Bracket IPv6 private addresses when combined with a port
- **cli:** Parse --agent-address as a real SocketAddr, not split(':')

## [0.1.0-nightly.20260908.5135508b] - 2026-09-08

### Added

- **web:** Add @temps-sdk/op operator design-system package
- **design-system:** Add the v5 design-system sandbox, guide and docs
- **design-system:** Kind icons wherever they add context
- **web:** Fmt helpers, Field errors, FormErrors, chart legend and table view, design tokens file
- **web:** Date, time, range, duration and schedule fields
- **web:** Agent, data-viz primitives, running state and night tokens
- **design-system:** Logs explorer across applications
- **web:** Skin scrollbars
- **design-system:** A copy answers on the control, never in a toast
- **design-system:** Landing header links to the Temps Cloud status page
- **design-system:** Resources monitoring on nodes and services
- **design-system:** The URL is the state
- **design-system:** Prose classes and the Article template
- **cloud:** Add secure managed telemetry client
- **cloud:** Add managed cloud link lifecycle and console settings
- **console:** Find Temps Cloud from the command palette
- **cloud:** Integrate managed alert routing
- **cloud:** Declare backup restore artifacts
- **cloud:** Stream managed backups and AI access
- **cloud:** Report instance location metadata
- **multi-node:** Initialize and rotate cluster CA safely
- **security:** Harden cluster CA rotation
- **agent:** Add systemd service installer
- **cloud:** Auto-provision, rotate, and audit the managed backup credential
- **web:** Badge and guard Cloud-managed S3 sources in the UI
- **cloud-client:** Cloud telemetry read proxy client (spike, ADR-040)
- **otel:** ADR-040 Phase A - cloud telemetry fidelity tiers and backfill
- **otel:** ADR-041 - Cloud-primary telemetry writes
- **otel,cloud:** ADR-042 one-click Cloud telemetry activation
- **cloud:** Thread session tokens through managed backup credentials
- **cloud:** Make ClickHouse entities and backups cloud-primary (ADR-043)
- **cloud-protocol:** Add backup lifecycle event wire types ([#912](https://github.com/gotempsh/temps/issues/912))
- **cloud:** Push backup lifecycle events to Cloud and webhooks
- **web:** Prototype AI-first onboarding workspace
- **web:** Model multi-project AI application stacks
- **ai:** Add application-first workspace
- **ai:** Start applications with a harness thread
- **ai:** Complete persistent workspace workflow
- **ai:** Add sandbox harness maintenance commands
- **ai:** Add persistent workspace file explorer
- **ai:** Add workspace file import and transfer
- **ai:** Add bounded workspace file transfers
- **providers:** Expose each service's continuous-archive S3 source

### Build

- **mariadb:** Publish WAL-G image through GHCR

### Documentation

- **agents:** Add the temps-design-system skill and point CLAUDE.md and AGENTS.md at it
- **design-system:** Content, forms, notifications, data viz, tokens, motion, icons and locale guidelines
- **design-system:** Dates, times and ranges in forms
- **design-system:** Generative UI, data viz forms, build-a-screen gallery, night rules
- **design-system:** Calendar heatmap section shows its demo
- **design-system:** Plugin UI, publish-ready package, review fixes

### Fixed

- **design-system:** One mark before a project name
- **design-system:** Drop the density command from the palette demo
- **design-system:** Session timeline axis draws inset ticks, not glyphs
- **auth:** Track pending-MFA-session origin so SSO enforcement can tell it apart from password login ([#918](https://github.com/gotempsh/temps/issues/918))
- **proxy:** Fall back to root index.html for asset-only directories ([#926](https://github.com/gotempsh/temps/issues/926))
- **email:** Scaleway domain verification, opt-in delete, and provider domain import picker ([#925](https://github.com/gotempsh/temps/issues/925))
- **migrations:** Stop blanket-demoting managed-monitor ownership ([#927](https://github.com/gotempsh/temps/issues/927))
- **openapi:** Preserve canonical cloud schema diff
- **cloud:** Validate backup upload destinations
- **backups:** Make MariaDB WAL-G PITR E2E self-contained
- **cloud:** Harden managed data paths
- **cloud:** Preserve safe startup across restarts
- **cloud:** Persist in-flight telemetry across restarts
- **cloud:** Close security review findings
- **redis:** Report restore startup crash details
- **backups:** Reject deletion of retained sources
- **backups:** Strip Redis replication trailer
- **backups:** Pull digest-pinned sidecars
- **network:** Auto-detect underlay device instead of hardcoding eth0
- **e2e:** Provision immutable MariaDB recovery image
- **proxy:** Lower console fallback log severity
- **agent:** Derive overlay MTU from underlay
- **dns:** Avoid host resolver bind collision
- **network:** Join control plane to multi-node overlay
- **network:** Escape nftables baseline marker
- **network:** Enforce authoritative cluster CIDR
- **network:** Permit scoped VXLAN forwarding
- **network:** Match Docker VXLAN ingress path
- **network:** Gate control-plane kernel lifecycle test
- **cloud:** Restore cross-node services and MariaDB backups
- **network:** Preserve cross-node service return paths
- **network:** Reconcile upgraded firewall baselines
- **multi-node:** Finalize secure cluster onboarding
- **deployments:** Stop compose containers when deploy job fails
- **dns:** Prevent duplicate internal answers
- **multinode:** Route container lifecycle to owning worker
- **cloud-client:** Surface the real error while telemetry is dropping
- **cli:** Include temps-cloud crates in the server log level filter
- **config:** Preserve ADR-041/042 cloud fields not sent by every client
- **cloud-client:** Attribute health to a block reason instead of a placeholder
- **cloud:** Close credential-redaction and encoding gaps in session-token support
- **cloud:** Surface Cloud's actual reason instead of a generic message
- **otel:** Match Cloud-primary span reads to the ts DateTime64 column
- **cloud:** Keep Cloud's reason when a retryable backup call fails
- **cloud:** Route pg_dump-fallback Postgres backups to native mirror, not WAL-G ([#901](https://github.com/gotempsh/temps/issues/901))
- **backup:** Skip S3 lifecycle reconcile for unscheduled operator buckets ([#896](https://github.com/gotempsh/temps/issues/896))
- **cloud:** Stop one stuck backup from throttling Cloud reporting for all others ([#906](https://github.com/gotempsh/temps/issues/906))
- **providers:** Pin WAL-G archive source per Postgres service
- **providers:** Extend continuous archive source pinning to MariaDB
- **providers:** Stop health-monitor full-model save from reverting the continuous-archive pin
- **providers:** Close IDOR and two ordering bugs in archive repoint ([#907](https://github.com/gotempsh/temps/issues/907))
- **providers:** Restore safe default MariaDB image instead of hard-failing
- **cloud:** Tighten credential redaction in backup lifecycle notify
- **cloud:** Remove dead capability advertisement and unread snapshot field
- **otel:** Remove dead cloud-telemetry outbox code
- **network:** Remove dead code from the multi-node overlay subsystem
- **cloud:** Decrypt STS session token before building S3 client credentials
- **webhooks:** Require BackupsRead when a webhook's backup-event subscription persists across an update
- **backup:** Shell-escape PGDATA-derived paths interpolated into sh -c commands
- **backup:** Scope external-service backup capability to accessible services
- **network:** Drop only the offending peer instead of stalling all control-plane reconciliation
- **notifications:** Require write permission to send a test notification
- **cloud-client:** Pin backup uploads to Cloudflare R2 endpoints
- **cli:** Add two missing leaf commands to the audited command-tree inventory
- **migrations:** Derive rollback step count instead of hardcoding it
- **backup:** Make MariaDB PITR tests and the pitr-scenario request the WAL-G image
- **providers,web:** Add MariaDB docker_image schema default and surface silent form validation failures
- **ci:** Align mariadb-walg-image's setup-qemu-action pin with the rest of the repo
- **deps:** Update hickory-net to 0.26.2 for hickory-resolver 0.26.2 compat
- **web:** Pin MariaDB managed preset to the real published WAL-G image digest
- **web:** Activate AI conversation options
- **ai:** Harden application onboarding
- **web:** Clarify AI project planning
- **tooling:** Preserve typo checker allowlist
- **ai:** Close application workspace review findings
- **sandbox:** Enforce secure cross-platform egress
- **ai:** Deserialize list pagination directly
- **ai:** Route sandbox model relay securely
- **ai:** Open managed sandbox status
- **ai:** Relay sandbox MCP capabilities securely
- **ai:** Close workspace review findings
- **ci:** Repair AI workspace regression gates
- **ai:** Isolate sandbox preview authentication
- **ai:** Paginate only visible conversations
- **ai:** Preserve archived conversation pages
- **cloud:** Mirror Redis/MongoDB logical-fallback backups to Cloud
- **cloud:** Address Greptile review findings on the redis/mongo mirror fix
- **providers:** Default WAL health probe's host to localhost, add CLI command
- **backups:** Accept MariaDB WAL-G repository bases for PITR restores
- **e2e:** Keep DinD bridges out of the cluster compute pool
- **agents:** Support secure sandbox networks on older Docker

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#919](https://github.com/gotempsh/temps/issues/919))
- **merge:** Sync cloud funnel with main
- **api:** Regenerate Cloud integration clients
- **cloud:** Regenerate clients after main merge
- Add source attribution headers
- **web,cli:** Regenerate API clients and skill docs after merge
- **ai:** Retrigger PR checks

### Refactor

- **design-system:** Name the first published system v1
- **design-system:** One chrome for the guide and the reference pages
- **design-system:** Drop the plan and density knobs from the console header
- **design-system:** UseUrlState moves into @temps-sdk/op
- **design-system:** Rename the package to @temps-sdk/ds
- **design-system:** Point imports, docs and the lockfile at @temps-sdk/ds
- **cloud:** Require backup checksums
- **deployments:** Centralize container runtime operations

### Testing

- **cloud:** Prove bounded shutdown flush
- **backups:** Prove MariaDB PITR with immutable images
- **cloud:** Fix migration and degraded-state regressions
- **redis:** Stabilize restart-loop regression
- **e2e:** Stabilize command and preview journeys
- **network:** Preserve existing control-plane routes
- **ci:** Synchronize websocket capture assertion

## [0.1.0-nightly.20260907.647e4043] - 2026-09-06

### Added

- **deployments:** Diagnose Docker Hub rate limits, add registry mirror prefix ([#905](https://github.com/gotempsh/temps/issues/905))
- **proxy:** Support real client IPs behind Bunny CDN ([#917](https://github.com/gotempsh/temps/issues/917))

### Fixed

- **error-tracking:** Honor Sentry envelope event ID ([#911](https://github.com/gotempsh/temps/issues/911))
- **proxy:** Stop classifying HTTP 101 responses as errors ([#913](https://github.com/gotempsh/temps/issues/913))
- **presets:** Accept full 2xx range in Next.js container health poll ([#914](https://github.com/gotempsh/temps/issues/914))

## [0.1.0-nightly.20260906.48c2396e] - 2026-09-06

### Fixed

- **cli:** Repair management commands and issue 899 regressions ([#904](https://github.com/gotempsh/temps/issues/904))
- **agents:** Surface preview gateway failures ([#909](https://github.com/gotempsh/temps/issues/909))
- **email:** Fix Scaleway domain identity bugs, add domain import ([#902](https://github.com/gotempsh/temps/issues/902))

## [0.1.0-nightly.20260905.e6ab76fc] - 2026-09-04

### Added

- **web:** Make delete-confirmation name chip copyable ([#900](https://github.com/gotempsh/temps/issues/900))
- **projects:** Install services from templates ([#855](https://github.com/gotempsh/temps/issues/855))

### Fixed

- **services:** Preserve template and monitor ownership ([#903](https://github.com/gotempsh/temps/issues/903))

## [0.1.0-nightly.20260904.44d70f90] - 2026-09-03

### Added

- **vulnerability-scanner:** Make Trivy scanning opt-in per project, disabled by default ([#892](https://github.com/gotempsh/temps/issues/892))

### Fixed

- **console:** Streamline service actions and log onboarding ([#890](https://github.com/gotempsh/temps/issues/890))
- **backup:** Harden restore safety and health UX ([#889](https://github.com/gotempsh/temps/issues/889))
- **backups:** Create scoped schedules atomically ([#891](https://github.com/gotempsh/temps/issues/891))
- **backup:** Use a GUC-compatible recovery_target_time format for PITR ([#894](https://github.com/gotempsh/temps/issues/894))
- **ai-agent-cli:** Use constant-time comparison for MCP bridge token ([#893](https://github.com/gotempsh/temps/issues/893))
- **console:** Unify navigation and responsive page chrome ([#895](https://github.com/gotempsh/temps/issues/895))

## [0.1.0-nightly.20260903.31b49635] - 2026-09-02

### Added

- **backup:** Implement MariaDB backup + point-in-time recovery
- **routes:** Add live Traefik docker-label discovery, with opt-in TLS for discovered routes ([#857](https://github.com/gotempsh/temps/issues/857))

### Fixed

- **web:** Pin browserslist to patched version to clear bun audit ([#882](https://github.com/gotempsh/temps/issues/882))
- **backup:** Add native disaster recovery restore ([#881](https://github.com/gotempsh/temps/issues/881))
- **web:** Send comparator symbols when creating an alert rule ([#884](https://github.com/gotempsh/temps/issues/884))
- **web:** Send symbolic comparator values for service alert rules ([#883](https://github.com/gotempsh/temps/issues/883))
- **web:** Harden dogfood-reported UI bugs across alert rules, storage, and env vars ([#885](https://github.com/gotempsh/temps/issues/885))
- **deployments:** Honor configured port over image EXPOSE detection ([#887](https://github.com/gotempsh/temps/issues/887))
- **deployments:** Allow developers to cancel and pause their own deployments ([#886](https://github.com/gotempsh/temps/issues/886))
- **console:** Align deployment state and service UX ([#888](https://github.com/gotempsh/temps/issues/888))

### Performance

- **otel:** Bound relay forwarding memory ([#880](https://github.com/gotempsh/temps/issues/880))

## [0.1.0-nightly.20260902.6abf301e] - 2026-09-01

### Added

- **runtime:** Add Docker Compose deployment ([#841](https://github.com/gotempsh/temps/issues/841))
- **analytics:** Key-based ingest for apps Temps does not deploy (ADR-040) ([#865](https://github.com/gotempsh/temps/issues/865))
- **skills:** Support platform provisioning device login ([#875](https://github.com/gotempsh/temps/issues/875))

### Fixed

- **web:** Stale deploy status badge and Load Balancer heading mismatch ([#871](https://github.com/gotempsh/temps/issues/871))
- **presets:** Two autopack Dockerfile-generation bugs found while building jupyterlab/jupyterlab ([#769](https://github.com/gotempsh/temps/issues/769))
- **web:** Persist Docker registry settings ([#877](https://github.com/gotempsh/temps/issues/877))

### Testing

- **proxy:** Raise captcha PoW attempt cap to stop random CI flakes ([#872](https://github.com/gotempsh/temps/issues/872))

## [0.1.0-nightly.20260901.e9dcea78] - 2026-09-01

### Styling

- **otel:** Restyle pipeline status charts to match analytics/Metrics Explorer ([#869](https://github.com/gotempsh/temps/issues/869))

## [0.1.0-nightly.20260901.27cadb1d] - 2026-08-31

### Added

- **presets:** Detect Dockerfile exposed ports ([#856](https://github.com/gotempsh/temps/issues/856))
- **deployments:** Deploy static-preset git repos with zero container overhead ([#858](https://github.com/gotempsh/temps/issues/858))

### Fixed

- **projects:** Address project creation review findings ([#853](https://github.com/gotempsh/temps/issues/853))
- **ai-chat:** Discover page country analytics ([#868](https://github.com/gotempsh/temps/issues/868))
- **otel:** Support proto3 optional fields in release builds ([#867](https://github.com/gotempsh/temps/issues/867))
- **deployments:** Bound disk use during static-from-source deployment copy ([#866](https://github.com/gotempsh/temps/issues/866))

## [0.1.0-nightly.20260831.11f3e324] - 2026-08-30

### Added

- **deployments:** Filter and limit environment container metrics by deployment ([#843](https://github.com/gotempsh/temps/issues/843))
- **projects:** Improve database and environment setup ([#840](https://github.com/gotempsh/temps/issues/840))
- **cli:** Add services metrics CLI parity ([#850](https://github.com/gotempsh/temps/issues/850))

### Fixed

- **cli:** Replace Bun-only runtime APIs with Node equivalents ([#842](https://github.com/gotempsh/temps/issues/842))
- **web:** Remove redundant deployment success toasts ([#845](https://github.com/gotempsh/temps/issues/845))
- **git:** Support self-hosted GitLab repository discovery ([#846](https://github.com/gotempsh/temps/issues/846))
- **web:** Stop login form from resetting while the user is typing ([#844](https://github.com/gotempsh/temps/issues/844))
- **sandbox:** Preserve snapshot workspaces across backends ([#839](https://github.com/gotempsh/temps/issues/839))
- **proxy:** Fail fast with an actionable error when port 80/443 are already in use ([#847](https://github.com/gotempsh/temps/issues/847))
- **agent:** Bound worker memory during remote operations ([#851](https://github.com/gotempsh/temps/issues/851))
- **deployer:** Pull compose images before force-recreate teardown ([#852](https://github.com/gotempsh/temps/issues/852))

### Testing

- **cli:** Cover services metrics validation, reject non-positive IDs ([#854](https://github.com/gotempsh/temps/issues/854))

## [0.1.0-nightly.20260830.9a2637ec] - 2026-08-29

### Fixed

- **release:** Pin Linux release builds to ubuntu-22.04 for glibc compat ([#837](https://github.com/gotempsh/temps/issues/837))
- **auth:** Validate password character count instead of byte length ([#834](https://github.com/gotempsh/temps/issues/834))
- **web:** Trim api key name on creation and prevent double-submit ([#832](https://github.com/gotempsh/temps/issues/832))
- **monitoring:** Suppress crash/downtime alerts for paused deployments ([#835](https://github.com/gotempsh/temps/issues/835))
- **auth:** Don't treat transient DB errors as an expired session ([#838](https://github.com/gotempsh/temps/issues/838))

## [0.1.0-nightly.20260829.643d4315] - 2026-08-28

### Miscellaneous

- **cli:** Repin temps skills to @temps-sdk/cli@0.1.35 ([#836](https://github.com/gotempsh/temps/issues/836))

## [0.1.0-nightly.20260828.24861e9b] - 2026-08-27

### Added

- **notifications:** Add severity-based notification routes ([#822](https://github.com/gotempsh/temps/issues/822))

### Fixed

- **build:** Install python3 in the musl toolchain image ([#825](https://github.com/gotempsh/temps/issues/825))
- **docker:** Apk upgrade base packages in runtime images ([#829](https://github.com/gotempsh/temps/issues/829))
- **proxy:** Require Upgrade-Insecure-Requests for HTTP-origin page tracking ([#830](https://github.com/gotempsh/temps/issues/830))

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#823](https://github.com/gotempsh/temps/issues/823))

## [0.1.0-nightly.20260827.e265afab] - 2026-08-27

### Added

- **web:** Add URL-backed project pagination ([#818](https://github.com/gotempsh/temps/issues/818))

### Fixed

- **cli:** Close staged binary before self-update preflight ([#819](https://github.com/gotempsh/temps/issues/819))
- **ai-gateway:** Scope usage summary/by-provider to the caller ([#817](https://github.com/gotempsh/temps/issues/817))
- **cli:** Read options.filter instead of options.filters in api-query ([#821](https://github.com/gotempsh/temps/issues/821))

### Miscellaneous

- **legal:** Preserve source attribution in distributions ([#820](https://github.com/gotempsh/temps/issues/820))

## [0.1.0-nightly.20260827.a78b7ec0] - 2026-08-26

### Added

- **mcp:** Rebuild Temps as an MCP server (ADR-039)
- **agent,deployments,cli:** Harden cluster DNS and surface resolver health for troubleshooting ([#799](https://github.com/gotempsh/temps/issues/799))
- **otel:** Harden ingest pipeline reliability and observability ([#815](https://github.com/gotempsh/temps/issues/815))

### Fixed

- **status-page:** Delay first health check cycle to avoid proxy-startup race ([#814](https://github.com/gotempsh/temps/issues/814))
- **web:** Restore dark-mode border/input contrast for toggles and buttons ([#813](https://github.com/gotempsh/temps/issues/813))
- **providers:** Keep MongoDB credentials off mongosh's command line ([#816](https://github.com/gotempsh/temps/issues/816))
- **providers:** Stop sending unreachable connection strings to cross-node services ([#795](https://github.com/gotempsh/temps/issues/795))

### Performance

- **geo:** Memory-map the MaxMind databases instead of reading them into the heap ([#808](https://github.com/gotempsh/temps/issues/808))
- **proxy:** Stop spawning a thread and a runtime per background loop ([#809](https://github.com/gotempsh/temps/issues/809))
- **logs:** Locate the tail replay window by scanning backwards in 64 KB blocks ([#807](https://github.com/gotempsh/temps/issues/807))

## [0.1.0-nightly.20260826.0004c042] - 2026-08-25

### Added

- **cli:** Default temps setup to sslip.io when no wildcard domain is given ([#790](https://github.com/gotempsh/temps/issues/790))

### Fixed

- **domains:** Disable Pebble nonce fault injection ([#796](https://github.com/gotempsh/temps/issues/796))
- **proxy:** Preserve original IP in admin gate updates ([#805](https://github.com/gotempsh/temps/issues/805))
- **console:** Show tooltips in collapsed sidebar ([#806](https://github.com/gotempsh/temps/issues/806))
- **providers:** Make external service operations node-aware ([#810](https://github.com/gotempsh/temps/issues/810))
- **proxy:** Fail closed on unresolvable IP when a restriction policy exists ([#811](https://github.com/gotempsh/temps/issues/811))
- **proxy:** Harden static file serving and ingestion ([#812](https://github.com/gotempsh/temps/issues/812))
- **auth:** Close cross-tenant project-scoping gaps in five handlers ([#797](https://github.com/gotempsh/temps/issues/797))

### Miscellaneous

- **docker:** Bump musl builder toolchain from rust:1.94-alpine to rust:1.98-alpine

### Performance

- **cli:** Stream the upgrade download and extraction to disk ([#804](https://github.com/gotempsh/temps/issues/804))

## [0.1.0-nightly.20260825.c754dd77] - 2026-08-24

### Added

- **web:** Add latency/traffic tabs to API traffic chart ([#792](https://github.com/gotempsh/temps/issues/792))

### Fixed

- **deps:** Replace unmaintained get_if_addrs and document accepted RUSTSEC risks ([#778](https://github.com/gotempsh/temps/issues/778))
- **web:** Detect static-site environments by source_type, not preset ([#777](https://github.com/gotempsh/temps/issues/777))
- **screenshots:** Stop plugin registration blocking on Chrome availability check ([#789](https://github.com/gotempsh/temps/issues/789))
- **cli:** Read upgrade's --license-path from TEMPS_EE_LICENSE_PATH too ([#791](https://github.com/gotempsh/temps/issues/791))
- **web:** Stop environment metrics legend from listing every replaced container ([#793](https://github.com/gotempsh/temps/issues/793))
- **providers:** Retry transient Docker image pull failures ([#794](https://github.com/gotempsh/temps/issues/794))
- **auth:** Require MFA step-up + confirmation on destructive/sensitive actions ([#788](https://github.com/gotempsh/temps/issues/788))

## [0.1.0-nightly.20260824.ce99d856] - 2026-08-24

### Fixed

- **deployments:** Don't gate deploy completion on DNS ACK when cluster DNS is off ([#776](https://github.com/gotempsh/temps/issues/776))

## [0.1.0-nightly.20260824.29b81382] - 2026-08-23

### Fixed

- **web:** Render live password policy checklist in Account.tsx ([#768](https://github.com/gotempsh/temps/issues/768))
- **web:** Render accessible live password policy checklist in ResetPassword ([#762](https://github.com/gotempsh/temps/issues/762))

## [0.1.0-nightly.20260823.0ddf1660] - 2026-08-23

### Fixed

- **routes:** Close lost-wakeup race in route sync long-poll ([#763](https://github.com/gotempsh/temps/issues/763))

## [0.1.0-nightly.20260823.ed49a729] - 2026-08-22

### Fixed

- **web:** Handle fetch errors in EmailProviderDetail with retry state ([#758](https://github.com/gotempsh/temps/issues/758))
- **web:** Trim whitespace inputs and guard against double-submission in CreateS3Source ([#760](https://github.com/gotempsh/temps/issues/760))

## [0.1.0-nightly.20260822.02fb4bd8] - 2026-08-21

### Added

- **projects:** Allow renaming a project's display name
- **proxy:** Add an optional per-project/environment IP access extension point
- **proxy:** Let a standalone proxy enforce project IP rules too

### Fixed

- **projects:** Make rename atomic, audit the persisted name, reset form on project switch
- **projects:** Roll back port-change writes when the route reload cannot be signalled
- **projects:** Fail closed when a route-reload NOTIFY cannot be published
- **projects:** Report the rename from the service instead of inferring it
- **projects:** Keep the rename read-back inside the transaction that wrote it
- **projects:** Correct rollback claims, share name validation, gate Docker tests
- **routes:** Bound route-table staleness when the LISTEN connection dies silently
- **proxy:** Close the allow-all startup race on project_ip_gate_slot
- **ci:** Satisfy cargo fmt on PR #725
- **proxy:** Close the project-ip-gate startup race by construction, not a timeout

### Miscellaneous

- Satisfy clippy 1.98 lints across the workspace

### Refactor

- **projects:** Address review findings on the rename change

### Styling

- **proxy:** Satisfy cargo fmt in the IP gate slot handoff

### Testing

- **projects:** Pin the partial-update semantics the git-settings path relies on
- **proxy:** Register the IP gate slot in the plugin-registration test

## [0.1.0-beta.56] - 2026-08-21

### Added

- **Temps Cloud:** Connect a self-hosted instance to the optional managed control plane in two steps from Settings, mirror OpenTelemetry spans without putting the managed service on the ingest path, and surface connection, buffering, and credential health through the console and CLI.
- **providers:** Reset all accumulated `pg_stat_statements` statistics from the Query Performance page through a write-protected, audited API with explicit destructive-action confirmation.
- **providers:** `pg_stat_statements`-based slow-query monitoring for user-provisioned Postgres services — a dedicated "Query Performance" page (sortable, paginated, with a per-query detail view) alongside a `GET /external-services/{id}/pg-stat-statements/slow-queries` endpoint and a `temps services slow-queries --id <id>` CLI command ([#460](https://github.com/gotempsh/temps/pull/460))
- **providers:** self-service "Enable & Restart" action to load `pg_stat_statements` on standalone Postgres services that predate this feature — clustered/HA services are rejected with a clear error instead, since a blind single-container restart bypasses controlled failover ([#460](https://github.com/gotempsh/temps/pull/460))
- **web:** date/time range picker (15m/1h/24h/7d + custom) on the service Logs screen, and a `temps services logs --id <id>` CLI command with `--from`/`--to` filtering ([#460](https://github.com/gotempsh/temps/pull/460))
- **funnels:** Add time-range shortcuts and show exact completion seconds ([#750](https://github.com/gotempsh/temps/issues/750))
- **projects:** Always show a health indicator on the project card ([#747](https://github.com/gotempsh/temps/issues/747))

### Documentation

- Require DCO sign-off on every commit, add Code of Conduct ([#752](https://github.com/gotempsh/temps/issues/752))

### Fixed

- **projects:** Report the connected Git provider instead of guessing it ([#740](https://github.com/gotempsh/temps/issues/740))
- **web:** Open the highlighted command palette row on Enter ([#751](https://github.com/gotempsh/temps/issues/751))
- **proxy:** Scope the traffic aggregation window cap to unique counts ([#755](https://github.com/gotempsh/temps/issues/755))
- **web:** Handle mutation and fetch errors in ErrorGroupDetail and ErrorEventDetail ([#746](https://github.com/gotempsh/temps/issues/746))

### Testing

- Fix flaky Postgres/Chrome tests and make MongoDB health timeouts diagnosable ([#754](https://github.com/gotempsh/temps/issues/754))

## [0.1.0-nightly.20260821.cdad16f9] - 2026-08-20

### Added

- **flags:** Allow TEMPS_FLAGS_REFRESH_INTERVAL_MS to override the 30s poll ([#733](https://github.com/gotempsh/temps/issues/733))
- **web:** Searchable, refreshable project select on the Proxy page ([#734](https://github.com/gotempsh/temps/issues/734))

### Fixed

- **email:** Scope deployment delivery to authorized projects ([#685](https://github.com/gotempsh/temps/issues/685))
- **web:** Handle fetch errors in BackupDetail and ScheduleDetail with actionable error state. ([#737](https://github.com/gotempsh/temps/issues/737))
- **session-replay:** Stop duplicating and over-recording replay events ([#739](https://github.com/gotempsh/temps/issues/739))

## [0.1.0-nightly.20260820.bfa3b1e2] - 2026-08-19

### Added

- **projects:** Opt in to deploying a project from alternate sources ([#716](https://github.com/gotempsh/temps/issues/716))
- **settings:** Bound project resource overrides with operator ceilings ([#713](https://github.com/gotempsh/temps/issues/713))
- **otel:** Count and alarm on rate-limited/quota-exceeded ingest requests ([#730](https://github.com/gotempsh/temps/issues/730))
- **ai-chat:** Expand read allowlist with ~130 safe GET endpoints ([#732](https://github.com/gotempsh/temps/issues/732))

### CI

- **rust-tests:** Verify temps-captcha-wasm pkg/ is a reproducible build ([#731](https://github.com/gotempsh/temps/issues/731))

### Documentation

- **installation:** Pin wasm-pack to required version 0.13.1 ([#706](https://github.com/gotempsh/temps/issues/706))

### Fixed

- **compose:** Make sandbox capability denials and override rejections actionable ([#723](https://github.com/gotempsh/temps/issues/723))
- **security:** Close 14 open findings from the security review ([#709](https://github.com/gotempsh/temps/issues/709))
- **web:** Support RFC3339 Sentry timestamps ([#727](https://github.com/gotempsh/temps/issues/727))
- **cli:** Honor --data-dir when starting the proxy alongside the console ([#724](https://github.com/gotempsh/temps/issues/724))
- **funnels:** Match CustomData filters against the props column ([#729](https://github.com/gotempsh/temps/issues/729))

### Performance

- **cli:** Build the serve runtime once, before the database pool ([#722](https://github.com/gotempsh/temps/issues/722))
- **geo:** Load the GeoLite2 city database once per process ([#721](https://github.com/gotempsh/temps/issues/721))

## [0.1.0-nightly.20260819.c2d15ad4] - 2026-08-18

### Added

- **monitoring:** Alert when file descriptors/sockets near exhaustion ([#712](https://github.com/gotempsh/temps/issues/712))
- **web:** Deep-link into plugin routes from the console ([#720](https://github.com/gotempsh/temps/issues/720))

### Fixed

- **monitoring:** Normalise uncapped container CPU alerts against host cores ([#714](https://github.com/gotempsh/temps/issues/714))
- **proxy:** Keep tracking page views when Fetch Metadata is absent ([#711](https://github.com/gotempsh/temps/issues/711))
- **web:** Hide Git configuration for projects with no repository ([#717](https://github.com/gotempsh/temps/issues/717))
- **proxy:** Honour the configured preview gateway host port ([#719](https://github.com/gotempsh/temps/issues/719))

## [0.1.0-nightly.20260818.86db3048] - 2026-08-17

### Added

- **onboarding:** Surface the next setup step
- **onboarding:** Add navigable copyable setup prompts
- **email:** Generate DMARC recommendation, fix DNS TXT-record verification precision ([#304](https://github.com/gotempsh/temps/issues/304))
- **analytics:** Select page timeframe from charts ([#696](https://github.com/gotempsh/temps/issues/696))
- **deployments:** Per-project Docker image retention with nightly pruning ([#172](https://github.com/gotempsh/temps/issues/172))
- **cli:** Deploy a drop archive into an existing project ([#703](https://github.com/gotempsh/temps/issues/703))

### Documentation

- **readme:** Fix star history chart across all locales ([#690](https://github.com/gotempsh/temps/issues/690))

### Fixed

- **projects:** Show traffic and deployment media
- **ui:** Keep preset icons visible in dark mode
- **monitoring:** Separate and widen alert rules
- **onboarding:** Anchor checklist navigation controls
- **projects:** Rollback failed domain reassignments
- **ci:** Grant missing permissions for dependency-scan scheduled/manual runs ([#691](https://github.com/gotempsh/temps/issues/691))
- **proxy:** Bucket projects-health summary hours without toUnixTimestamp64Milli ([#698](https://github.com/gotempsh/temps/issues/698))
- **providers:** Allowlist pgvector images, and let operators extend the list ([#697](https://github.com/gotempsh/temps/issues/697))
- **docker:** Bump Alpine base images off EOL 3.19/3.20 to 3.22 ([#692](https://github.com/gotempsh/temps/issues/692))
- **session-replay:** Batch event inserts instead of one round-trip per event ([#701](https://github.com/gotempsh/temps/issues/701))
- **dashboard:** Correct sessions and project navigation ([#700](https://github.com/gotempsh/temps/issues/700))
- **migrate:** Show post-migration maintenance progress and cancel the right backend ([#707](https://github.com/gotempsh/temps/issues/707))
- **proxy:** Redact credentials from persisted proxy logs ([#699](https://github.com/gotempsh/temps/issues/699))
- **compose:** Deliver project secrets to Docker Compose stacks ([#702](https://github.com/gotempsh/temps/issues/702))
- **skills:** Repin the temps skill to the published 0.1.34 integrity ([#710](https://github.com/gotempsh/temps/issues/710))

### Miscellaneous

- **cli:** Release @temps-sdk/cli v0.1.34 and repin skills ([#708](https://github.com/gotempsh/temps/issues/708))

### Performance

- **proxy:** Bound preview limiter flood work

## [0.1.0-nightly.20260817.651f4a16] - 2026-08-16

### Added

- **web:** Redesign project navigation
- **web:** Simplify platform sidebar
- **web:** Restore monitoring workspace
- **web:** Prioritize AI activation in onboarding
- **web:** Verify AI harness activation
- **platform:** Expose feature maturity labels
- **web:** Streamline AI harness onboarding
- **web:** Add platform tool section icons
- **web:** Redesign project list

### Documentation

- **readme:** Tag temps.sh links with UTM parameters ([#688](https://github.com/gotempsh/temps/issues/688))
- **cli:** Sync analytics command references

### Fixed

- **deps:** Patch event-listener RustSec finding ([#97](https://github.com/gotempsh/temps/issues/97))
- **environments:** Allow clearing node placement
- **deployments:** Treat an empty target_nodes list as unconstrained
- **security:** Close final placement and preview gaps
- **providers:** Restore redis database allocation isolation ([#683](https://github.com/gotempsh/temps/issues/683))
- **ai-gateway:** Allow re-enabling a disabled provider model via add ([#687](https://github.com/gotempsh/temps/issues/687))
- **ci:** Keep tag releases from writing Actions caches ([#686](https://github.com/gotempsh/temps/issues/686))
- **web:** Widen service creation layout
- **web:** Clarify platform tools group label
- **platform:** Address navigation redesign review blockers
- **security:** Scope redesign resource access

### Refactor

- **web:** Replace monitoring page with proxy
- **web:** Group platform navigation by intent
- **web:** Promote git providers in navigation
- **web:** Open monitoring on alerts
- **web:** Separate AI harness onboarding
- **web:** Move AI harness out of sidebar

### Testing

- **e2e:** Focus multinode scenario on worker deployment

## [0.1.0-nightly.20260816.3bf831a1] - 2026-08-16

### Added

- **analytics:** Add generic API traffic drilldowns ([#669](https://github.com/gotempsh/temps/issues/669))
- **otel:** Span attribute facets for fast filtering at scale
- **otel:** Production-harden facet backfill + add TimescaleDB parity
- **otel,cli:** Regenerate API clients for facet status/retry, add CLI retry
- **proxy:** Add scoped API traffic drilldowns ([#675](https://github.com/gotempsh/temps/issues/675))

### Documentation

- **providers:** Describe structured MariaDB filters
- **e2e:** Describe HA probe callers

### Fixed

- **otel:** Harden claimed project slug diagnostics ([#666](https://github.com/gotempsh/temps/issues/666))
- **presets:** Upgrade Autopack Python source builds ([#667](https://github.com/gotempsh/temps/issues/667))
- **projects:** Stop mislabeling git connection errors as project-not-found ([#668](https://github.com/gotempsh/temps/issues/668))
- **web:** Chart deploy-marker overlap and Cmd+K palette drift ([#671](https://github.com/gotempsh/temps/issues/671))
- **projects:** Scope env detection and improve repository connections ([#665](https://github.com/gotempsh/temps/issues/665))
- **projects:** Harden shared git settings follow-up ([#673](https://github.com/gotempsh/temps/issues/673))
- **analytics:** Cast ClickHouse traffic percentiles ([#672](https://github.com/gotempsh/temps/issues/672))
- **security:** Harden critical trust boundaries
- **security:** Bundle high severity hardening
- **security:** Address integration review findings
- **security:** Close residual tenant identity races
- **security:** Centralize tenant hostname claims
- **security:** Canonicalize tenant identity claims
- **security:** Isolate proxy inspection quotas
- **ci:** Align tests with security hardening
- **security:** Close upgrade and placement bypasses
- **security:** Validate upgrade recovery paths
- **security:** Fail closed on invalid placement
- **security:** Enforce constrained scheduling
- **tests:** Use supported postgres images in upgrade guard
- **tests:** Use an allowlisted postgres image that actually exists
- **tests:** Use allowlisted postgres image for image-update tests
- **providers:** Probe walg.env from the volume when no live container exists
- **proxy:** Scope production-HTTPS default to resolved project traffic
- **e2e:** Enroll multinode workers over https
- **e2e:** Emit portable probe images
- **e2e:** Use valid control-plane probe placement
- **otel:** Address review findings on facet backfill hardening
- **auth:** Block admin MFA enrollment during password login ([#678](https://github.com/gotempsh/temps/issues/678))
- **deployments:** Enforce project access on cron reads ([#679](https://github.com/gotempsh/temps/issues/679))
- **providers:** Constrain legacy managed service sweep ([#680](https://github.com/gotempsh/temps/issues/680))

### Miscellaneous

- **cli,web:** Regenerate API clients against the merged backend
- **cli,web:** Regenerate API clients after second main merge

### Performance

- **ci:** Inherit trusted nextest target cache ([#674](https://github.com/gotempsh/temps/issues/674))

### Styling

- **providers:** Satisfy rustfmt on wal-probe test helper call
- **proxy:** Satisfy rustfmt on new production-https tests

### Testing

- **providers:** Use managed postgres images
- **e2e:** Expect cross-project DNS isolation

## [0.1.0-nightly.20260815.a23a4903] - 2026-08-14

### Added

- **web:** Redesign project deployment actions ([#660](https://github.com/gotempsh/temps/issues/660))
- **analytics:** Add AI-powered API traffic insights ([#661](https://github.com/gotempsh/temps/issues/661))
- **cli:** Expose performance insights filters ([#662](https://github.com/gotempsh/temps/issues/662))

### Fixed

- **web:** Show branch selector on Connect repository page ([#657](https://github.com/gotempsh/temps/issues/657))
- **analytics:** Preserve events backfill progress output ([#658](https://github.com/gotempsh/temps/issues/658))
- **proxy:** Add per-project/environment concurrent-connection cap ([#655](https://github.com/gotempsh/temps/issues/655))
- **git:** Share live preset/env-example/compose fetches across users ([#659](https://github.com/gotempsh/temps/issues/659))
- **web:** Simplify project header links ([#663](https://github.com/gotempsh/temps/issues/663))
- **otel:** Include project slug in auth warnings ([#664](https://github.com/gotempsh/temps/issues/664))

## [0.1.0-nightly.20260814.af018b31] - 2026-08-13

### Added

- **web:** Surface cluster DNS toggle in Worker Nodes settings ([#647](https://github.com/gotempsh/temps/issues/647))
- **cli:** Add metrics command to query OTel project metrics ([#649](https://github.com/gotempsh/temps/issues/649))
- **web:** Add copy URL on project deployment information ([#651](https://github.com/gotempsh/temps/issues/651))
- **proxy:** Add per-project latency percentiles and custom time ranges ([#652](https://github.com/gotempsh/temps/issues/652))
- **deployments:** Send redacted failure trace to Temps, or open a GitHub issue ([#653](https://github.com/gotempsh/temps/issues/653))

### Build

- **deps:** Bump axum-test from 18.7.0 to 21.0.0 ([#614](https://github.com/gotempsh/temps/issues/614))
- **deps:** Bump schemars from 0.8.22 to 1.2.1 ([#615](https://github.com/gotempsh/temps/issues/615))
- **deps:** Bump totp-rs from 5.7.2 to 6.0.0 ([#607](https://github.com/gotempsh/temps/issues/607))
- **deps:** Bump jsonwebtoken from 10.4.0 to 11.0.0 ([#608](https://github.com/gotempsh/temps/issues/608))

### Fixed

- **proxy:** Keep RouteTableListener alive in split-mode proxy ([#644](https://github.com/gotempsh/temps/issues/644))
- **cli:** Watch the newly triggered deployment, not the previous one ([#637](https://github.com/gotempsh/temps/issues/637))
- **deployments:** Reclaim Docker build cache and old deployment images ([#645](https://github.com/gotempsh/temps/issues/645))
- **web:** Re-measure AI composer height once the dock's open transition settles ([#654](https://github.com/gotempsh/temps/issues/654))
- **database:** Replace testcontainers flat-sleep with real postgres readiness wait ([#648](https://github.com/gotempsh/temps/issues/648))
- **git:** Make git connections shared across all users ([#656](https://github.com/gotempsh/temps/issues/656))

### Miscellaneous

- **cli:** Bump @temps-sdk/cli to 0.1.33 ([#650](https://github.com/gotempsh/temps/issues/650))

## [0.1.0-nightly.20260813.e22f5ddf] - 2026-08-12

### Added

- **proxy:** Configurable request timeouts (global ceiling + per-project override, SSE/WebSocket-aware) ([#642](https://github.com/gotempsh/temps/issues/642))

## [0.1.0-nightly.20260812.86c4c52e] - 2026-08-12

### Added

- **deployments:** Make public readiness check configurable per project/environment
- **cli:** Add public readiness timeout/disable flags to projects config
- **skills:** Add Temps workflow router
- **sandbox:** Sandbox snapshots — take/restore for Docker backend (ADR-037) ([#622](https://github.com/gotempsh/temps/issues/622))
- **skills:** Add start-temps dev skill for local server bootstrap ([#634](https://github.com/gotempsh/temps/issues/634))
- **ai:** Add subscription-backed agent CLI provider foundation
- **ai:** Add provider-aware interactive chat controls

### CI

- **skills:** Secure Temps router

### Fixed

- **cli:** Require target context in write examples
- **deployer:** Require published ports reachable before compose readiness
- **cli:** Pass detected composePath through on drop deploys
- **deployments:** Guard TempDirGuard against removing paths outside safe temp roots ([#633](https://github.com/gotempsh/temps/issues/633))
- **web:** Match new-project repo picker to compact project-settings design
- **skills:** Harden Temps router generation
- **skills:** Contain capability detector reads
- **skills:** Ignore blocking special files ([#638](https://github.com/gotempsh/temps/issues/638))
- **ci:** Fix three unrelated main CI failures ([#632](https://github.com/gotempsh/temps/issues/632))
- **status-page:** Don't collapse degraded monitors into project-down status ([#640](https://github.com/gotempsh/temps/issues/640))
- **deployments:** Remove the always-broken public-readiness rollback gate ([#641](https://github.com/gotempsh/temps/issues/641))
- **ai:** Harden AgentCliAiService against security review findings
- **web:** Restore global AI chat access
- **ai:** Stabilize cli provider chat
- **ai:** Refresh chat model capabilities
- **ai:** Make harness controls and tools reliable
- **ai:** Render API timestamps as dates in answers
- **ai:** Isolate subscription chat tool runtime
- **ai:** Enforce provider runtime boundaries

### Performance

- **otel:** Fix unbounded ClickHouse scans ([#639](https://github.com/gotempsh/temps/issues/639))

### Refactor

- **ai:** Unify provider capabilities and turn runtime

### Testing

- **database:** Derive latest migration in round-trip test
- **ai:** Add infrastructure prompt catalog

## [0.1.0-nightly.20260812.73dbd99e] - 2026-08-12

### Added

- **external-plugins:** Add caller-scoped platform API
- **compose:** Make Git deployments usable end to end ([#592](https://github.com/gotempsh/temps/issues/592))
- **error-tracking:** Add same-origin Sentry tunnel for browser SDKs
- **self-update:** Apply pending migrations before restarting ([#629](https://github.com/gotempsh/temps/issues/629))

### Build

- **deps:** Bump docker/setup-buildx-action from 3 to 4 ([#604](https://github.com/gotempsh/temps/issues/604))
- **deps:** Bump nanoid from 3.3.18 to 6.0.1 in /web ([#601](https://github.com/gotempsh/temps/issues/601))
- **deps:** Bump tower from 0.4.13 to 0.5.3 ([#611](https://github.com/gotempsh/temps/issues/611))
- **deps:** Bump the web-minor-patch group across 1 directory with 12 updates
- **deps:** Bump tokio-tungstenite from 0.29.0 to 0.30.0
- **deps:** Bump the cargo-minor-patch group across 1 directory with 8 updates
- **deps:** Bump actions/checkout from 5 to 7
- **deps:** Bump pem from 3.0.6 to 4.0.0 ([#612](https://github.com/gotempsh/temps/issues/612))
- **deps:** Bump tower-http from 0.6.11 to 0.7.0 ([#613](https://github.com/gotempsh/temps/issues/613))
- **deps:** Bump octocrab from 0.49.9 to 0.54.1 ([#610](https://github.com/gotempsh/temps/issues/610))
- **deps:** Bump framer-motion from 12.43.0 to 13.0.0 in /web

### Documentation

- **cli:** Restructure skill references
- **cli:** Use pinned zero-install commands

### Fixed

- **web:** Redirect to login on expired session instead of hanging on stale screen
- **error-tracking:** Stop leaking DB error detail on tunnel path, fix wildcard route resolution
- **proxy:** Canonicalize Google and Bing crawler names ([#625](https://github.com/gotempsh/temps/issues/625))
- **proxy:** Stop leaking raw HTML under a text/markdown Content-Type ([#626](https://github.com/gotempsh/temps/issues/626))
- **domains:** Persist request_challenge renewal failures, add attempt history
- **deployments:** Clean up leaked /tmp/temps-deployments dirs on download failure ([#630](https://github.com/gotempsh/temps/issues/630))
- **cli:** Generate complete Node-compatible command docs
- **ci:** Pin readiness probes to local proxy
- **deployments:** Probe HTTPS routes via local HTTP listener

### Miscellaneous

- **sdk:** Regenerate CLI and web SDKs after merging main
- **cli:** Release v0.1.31
- **web:** Remove framer-motion dependency

### Testing

- **e2e:** Verify internal application and database DNS ([#619](https://github.com/gotempsh/temps/issues/619))
- Align GitHub and migration expectations
- **e2e:** Wait for deployment completion

## [0.1.0-nightly.20260811.7f6037c7] - 2026-08-10

### Added

- **ai-agents:** Classify plain Googlebot as Google/Mixed
- **cli:** Add otel-forward commands for OTLP relay destinations ([#591](https://github.com/gotempsh/temps/issues/591))

### Documentation

- **security:** Design PostgreSQL datasource TLS trust ([#337](https://github.com/gotempsh/temps/issues/337))

### Fixed

- **ai-agents:** Add ClickHouse backfill counterpart, closes coverage gap
- **cli:** Align apikeys create role vocabulary with backend Role enum ([#616](https://github.com/gotempsh/temps/issues/616))
- **analytics:** Show real duration on bounce/exit pageviews instead of blank ([#618](https://github.com/gotempsh/temps/issues/618))

### Testing

- **e2e:** Close remaining scenario gaps (mariadb restore, env vars, api keys) ([#599](https://github.com/gotempsh/temps/issues/599))
- **e2e:** Add multinode mTLS coverage and scenario CI ([#617](https://github.com/gotempsh/temps/issues/617))

## [0.1.0-nightly.20260810.ccf9c612] - 2026-08-10

### Added

- **e2e:** Otel-quota-scenario + fix silently-inert OTel storage quota ([#586](https://github.com/gotempsh/temps/issues/586))
- **providers:** MongoDB restore_in_place, restore_to_new_service, restore_capabilities ([#596](https://github.com/gotempsh/temps/issues/596))
- **redis:** Implement restore_capabilities, restore_in_place, restore_to_new_service ([#597](https://github.com/gotempsh/temps/issues/597))
- **restore:** S3/MinIO managed-service restore_in_place ([#595](https://github.com/gotempsh/temps/issues/595))

### Fixed

- Correct dual-license metadata and stale redirect status doc ([#598](https://github.com/gotempsh/temps/issues/598))

### Testing

- **e2e:** Add pitr-scenario for point-in-time postgres recovery ([#585](https://github.com/gotempsh/temps/issues/585))
- **e2e:** Deploy-lifecycle-scenario (rollback/pause/resume/promote) + fix 3 real bugs ([#587](https://github.com/gotempsh/temps/issues/587))
- **e2e:** Add db-ha-failover-scenario for Postgres HA/pg_auto_failover ([#588](https://github.com/gotempsh/temps/issues/588))
- **e2e:** Add pg-upgrade-scenario for Postgres major-version upgrades ([#593](https://github.com/gotempsh/temps/issues/593))

## [0.1.0-nightly.20260809.a6a711b9] - 2026-08-08

### Added

- **projects:** Mark environment variables as secrets at project creation
- **otel:** Add OtelRelay extension point for telemetry forwarding plugins ([#583](https://github.com/gotempsh/temps/issues/583))
- **teams:** Add ProjectAccessChecker::invalidate_permissions_cache ([#589](https://github.com/gotempsh/temps/issues/589))

### Fixed

- **security:** Harden postgres credential transport
- **security:** Enforce private postgres consumers

### Styling

- **web:** Drop incidental prettier reformatting from the env-var change

### Testing

- **e2e:** DNS/TLS/email/CLI + observability/data-storage e2e coverage ([#582](https://github.com/gotempsh/temps/issues/582))

## [0.1.0-nightly.20260808.e02f5881] - 2026-08-07

### Added

- **sandbox:** Add persistent workspace sandboxes with wake-on-access
- **cli:** Resolve workspace source from project, repo, or local checkout
- **sandbox:** Add interactive terminal and `sandbox shell`
- **web:** Regenerate the SDK and surface workspaces in the console
- **deployer:** Satisfy compose env_file references automatically

### Build

- **cli:** Add spec:update to refresh openapi.json canonically

### CI

- **cli:** Enforce canonical openapi.json in a hook and in CI
- **sandbox:** Install protoc in the beta image prepare job

### Documentation

- **adr:** Add ADR-036 for persistent workspace sandboxes

### Fixed

- **settings:** Close the review findings on console updates
- **auth:** Only require step-up from users who have enrolled MFA
- **sandbox:** Restore terminal echo, work dir, and CLI response shapes
- **sandbox:** Address review findings on the terminal and step-up policy
- **sandbox:** Address review findings on workspaces and the terminal
- **sandbox:** Normalise the PTY from the host so terminals echo on any image
- **web:** Raise the js-yaml override past the advisory, and make detach honest
- **sandbox:** Repair compound --cmd, bound agent writes, surface oversized input
- **projects:** Normalize blank directory on settings and git updates
- **web:** Detect presets and prefill the URL on the public-repo path
- **auth:** Audit permission denials safely
- **auth:** Bound permission denial audit storage
- **security:** Bound query memory and audit data
- **security:** Reject oversized rows before encoding
- **security:** Redact audited data identifiers
- **cli:** Sanitize untrusted terminal output
- **cli:** Sanitize invalid filter errors
- **security:** Resolve PR review findings
- **security:** Enforce pre-wire Redis budgets
- **redis:** Bound aggregate admission work
- **security:** Close final review gaps
- **security:** Bound metadata scan work
- **redis:** Preserve bounded cursor paging
- **review:** Close final compatibility gap

### Miscellaneous

- **cli:** Regenerate openapi spec and client
- **sdk:** Release analytics-core v0.0.3, react-analytics v0.0.5, analytics-browser v0.0.3, svelte-analytics v0.0.2, vue-analytics v0.0.2 ([#575](https://github.com/gotempsh/temps/issues/575))
- **cli:** Release v0.1.30, release node-sdk v0.0.7 ([#574](https://github.com/gotempsh/temps/issues/574))

### Styling

- **cli:** Canonicalize openapi.json key order and formatting

### Testing

- **e2e:** Make console/CLI e2e suite parallel-safe, add API-key coverage

## [0.1.0-nightly.20260807.146be0c2] - 2026-08-06

### Added

- **analytics:** Crawler opt-in for breakdowns, and prune NULL keys early
- **settings:** Apply releases and restart from the console
- **settings:** Version page, release channels, and install without a restart
- **providers:** Read-only GET rows endpoint and per-service AI data access opt-in
- **cli:** Add 'temps data' for read-only browsing of service data
- **query-postgres,web:** Show views, list-level stats, and per-engine labels
- **web:** Independent tree scroll, column selection, and row detail panel
- **web:** Container overview, sidebar sizes, per-tab scroll, and S3 fixes
- **otel:** Rank operations by latency, volume, and variability
- **cli:** Add `traces span-stats` for operation latency
- **web:** Add an Operations view to Traces

### Documentation

- **analytics:** Declare include_crawlers on the timeline endpoint
- Require features to be discoverable and onboard when unconfigured
- **cli:** Document the data command group
- **skills:** Document the data command group in the temps-cli skill

### Fixed

- **analytics:** Classify self-referrals as Direct, not Referral
- **analytics:** Exclude crawler traffic from breakdowns and top pages
- **cli:** Make analytics overview locations and sparkline respect --period
- **analytics:** Populate visitor language instead of always "Unknown"
- **analytics:** Send language from the shared browser SDK core too
- **analytics:** Close review findings on attribution, language and CH parity
- **cli:** Reject release tags that escape the temps repository
- **web:** Restore deep paths on reload and compact the data browser
- **providers:** Clamp row limit for every caller and cover the AI gate with tests
- **web:** Drop a sort field the current table does not have
- **query-postgres,web:** Count views in entity_count, inline the entity breadcrumb
- **security:** Close connection-string injection, SQL denylist bypass, and markdown exfiltration
- **security:** Share one SQL validator, close MariaDB bypasses and a remote panic
- **security:** Close the nine findings from the data-browser audit
- **security:** Close the review findings, and make the TLS ladder actually work
- **security:** Close the re-audit findings, including a UNION injection I added
- **web:** Give sole create actions an N shortcut and guard it behind overlays
- **web:** Rank palette results by relevance, add Teams, resizable data-browser tree
- **web:** Show unified-trace projects as a colour legend, not a slug per span
- **web:** Make Back work on deep-linked pages
- **web:** Address self-review findings on the shortcut hook and project dot
- **web:** Order sibling spans correctly and show exact durations
- **config,proxy:** Stop settings writes from wiping the admin gate
- **proxy:** Close fail-open paths in the admin gate reload
- **ci:** Unbreak the two checks that are red on every PR
- **otel:** Bound span-stats project count and time window

### Miscellaneous

- Use neutral placeholder names in examples and comments
- **scripts:** Seed OTel traces across projects and services
- **scripts:** Replay a captured trace into a local instance

### Performance

- **query-postgres:** Use planner stats for row count, and report table size
- **query:** Avoid full scans for MariaDB row counts, add MongoDB size

### Revert

- **web:** Drop the data-browser tree navigateTo routing

## [0.1.0-nightly.20260806.c64e8f98] - 2026-08-06

### Added

- **console:** Improve user and project onboarding ([#552](https://github.com/gotempsh/temps/issues/552))
- **flags:** Multi-language integration guide, and shiki-highlighted code blocks ([#556](https://github.com/gotempsh/temps/issues/556))
- **env-vars:** Convert an existing variable to a write-only secret ([#555](https://github.com/gotempsh/temps/issues/555))
- **dns:** Govern unattended provider automation ([#554](https://github.com/gotempsh/temps/issues/554))

### Fixed

- **git:** Stop invisible connections from blocking provider deletion ([#553](https://github.com/gotempsh/temps/issues/553))
- **telemetry:** Report git-describe version, not static Cargo.toml version ([#558](https://github.com/gotempsh/temps/issues/558))
- **sandbox:** Free volumes and work dirs when a sandbox is destroyed ([#523](https://github.com/gotempsh/temps/issues/523))

## [0.1.0-nightly.20260805.c6bd08de] - 2026-08-04

### Added

- **presets:** Replace the nixpacks build engine with autopack ([#530](https://github.com/gotempsh/temps/issues/530))
- **auth:** Add step-up verification for sensitive actions ([#547](https://github.com/gotempsh/temps/issues/547))
- **flags:** Feature flags Phase 1 — backend, CLI, SDK and console UI ([#526](https://github.com/gotempsh/temps/issues/526))
- **ai:** Let the assistant propose metric alert rules from a project's own telemetry ([#521](https://github.com/gotempsh/temps/issues/521))
- **teams:** Teams and project-scoped RBAC in OSS ([#486](https://github.com/gotempsh/temps/issues/486))
- **drop:** Deploy uploaded source archives ([#549](https://github.com/gotempsh/temps/issues/549))

### Fixed

- **proxy:** Keep backend latency for streaming sessions ([#545](https://github.com/gotempsh/temps/issues/545))
- **web:** Advance traces time window on refresh and skip it for trace-id search ([#550](https://github.com/gotempsh/temps/issues/550))
- **deps:** Patch aiohttp and cryptography Dependabot advisories ([#551](https://github.com/gotempsh/temps/issues/551))
- **otel:** Bound ingest memory under exporter bursts ([#544](https://github.com/gotempsh/temps/issues/544))

## [0.1.0-nightly.20260804.c51ac6c2] - 2026-08-03

### Added

- **providers:** Reset pg_stat_statements statistics ([#527](https://github.com/gotempsh/temps/issues/527))
- **sandbox:** Mint shareable preview links with expiring session grants ([#525](https://github.com/gotempsh/temps/issues/525))
- **telemetry:** Add deployment failure taxonomy and template context ([#546](https://github.com/gotempsh/temps/issues/546))

### Fixed

- **web:** Stop env var edit modal showing blank value after save ([#524](https://github.com/gotempsh/temps/issues/524))
- **web:** Add icons and prioritize projects in command search
- **deps:** Bump brace-expansion override to ^5.0.9 for GHSA-rgw5-rvv9-x895 ([#548](https://github.com/gotempsh/temps/issues/548))

### Miscellaneous

- **deps-dev:** Bump @types/react-dom in /web in the react group ([#528](https://github.com/gotempsh/temps/issues/528))

### Refactor

- **web:** Move project setup into project sidebar ([#541](https://github.com/gotempsh/temps/issues/541))

## [0.1.0-nightly.20260803.ae960395] - 2026-08-02

### Added

- **providers:** Warn when a cluster has no node accepting writes
- **audit:** Support events whose actor has no resolvable account ([#411](https://github.com/gotempsh/temps/issues/411))
- **audit:** Record failed logins and rejected MFA codes ([#412](https://github.com/gotempsh/temps/issues/412))
- **environments:** Per-environment HTTP→HTTPS redirect override, with an unconditional ACME bypass ([#522](https://github.com/gotempsh/temps/issues/522))
- **security:** Block cloud-metadata egress from app containers

### CI

- Fix the cache-budget leak that makes Compose Security take 92 minutes ([#516](https://github.com/gotempsh/temps/issues/516))

### Fixed

- **providers:** Make cluster creation survive bad placement and stay retryable
- **deployments:** Write the clone failure into the deploy log
- **projects:** Reject repo changes that would strand the clone URL
- **web:** Stop claiming a project deployed when its only run failed
- **providers:** Treat unreachable cluster nodes as leaderless
- **proxy:** Exclude streaming sessions from latency metrics ([#514](https://github.com/gotempsh/temps/issues/514))
- **domains:** Validate custom-domain input and scope it to the caller's project ([#515](https://github.com/gotempsh/temps/issues/515))
- **cli:** Repo lookup by search, nested group paths, and --secret for env vars ([#517](https://github.com/gotempsh/temps/issues/517))
- **core:** Make Visit links reachable on sslip.io installs ([#488](https://github.com/gotempsh/temps/issues/488))
- **web:** Show a back button in the collapsed sidebar's sub-navigation ([#520](https://github.com/gotempsh/temps/issues/520))
- **git:** Keep clone credentials out of errors
- **auth:** Remove code-execution permissions from PlatformAdmin
- **auth:** Close platform admin sandbox creation gap
- **web:** Make the AI chat page fill the content area exactly ([#518](https://github.com/gotempsh/temps/issues/518))
- **providers:** Persist inferred parameters on the plugin init path
- **providers:** Check loopback when picking host ports, and report real service health
- **providers:** Route MariaDB port selection through the shared finder
- **providers:** Persist cluster topology atomically
- **email:** Close SSRF hole and DNS-rebinding gap in SMTP deliverability probe
- **email:** Complete SMTP SSRF hardening
- **email:** Keep validation configuration database-ready
- **email:** Close remaining SMTP proxy SSRF gaps
- **security:** Make metadata egress blocking atomic

### Testing

- **web:** Verify project deployment labels

## [0.1.0-nightly.20260802.ba96c0f7] - 2026-08-02

### Added

- **deployer:** Attach app containers to required extra Docker networks ([#501](https://github.com/gotempsh/temps/issues/501))
- **deployments:** Support worker nodes of a different architecture
- **deployments:** Make cross-architecture builds opt-in
- **deployments:** Report skipped nodes and refuse impossible replica counts
- **projects:** Expose cross-architecture builds in the deployment config API

### Documentation

- **multi-node:** Document opt-in cross-builds and replica shortfall
- **multi-node:** Correct the config key and regenerate the SDK

### Fixed

- **web:** Align react with react-dom at 19.2.8 and guard against future drift ([#505](https://github.com/gotempsh/temps/issues/505))
- **deployments:** Reject cross-builds the legacy Docker builder mislabels
- **deployments:** Never record a guessed node architecture
- **deployments:** Close two paths that still trusted a guessed platform
- **deployments:** Bound cross-build targets and record per-replica images
- **deployments:** Act only on a confirmed control-plane platform
- **deployments:** Discover the daemon platform on paths that never build
- **deployer:** Keep the ARM variant through discovery and verification
- **deployments:** Degrade instead of failing the cluster's builds
- **deployments:** Check replica shortfall before the local-only fallback
- **deployments:** Record the peer address on architecture-change audits
- **deployments:** Clean up containers before owner deletion
- **deployments:** Remove unsafe cross-instance orphan sweep
- **providers:** Derive managed service container names in one place ([#503](https://github.com/gotempsh/temps/issues/503))

### Testing

- **web:** Add console end-to-end UI tests, and fix the post-login 404 they found ([#511](https://github.com/gotempsh/temps/issues/511))

## [0.1.0-nightly.20260801.324ad120] - 2026-08-01

### CI

- Run temps-otel and four other crates' integration tests

### Documentation

- **contributing:** Specify temps binary for running server

### Fixed

- **auth:** Gate user administration on a dedicated users:manage permission
- **auth:** Restrict audit:read to administration roles
- **web:** Gate audit-log UI to roles with audit:read
- **web:** Import Navigate from react-router, not react-router-dom
- **otel:** Restore exact span dedup after dropping FINAL
- **sdk:** Regenerate clients for the include_total contract
- **imports:** Make repository link optional, add Portainer TLS skip
- **cli:** Environments resources ignored -p, always resolving "Project not found"
- **providers:** Enforce project scoping for session/API-key/CLI callers
- **import:** Deploy docker-source imports for real instead of leaving them pending
- **domains:** Reserve the console hostname from project domains
- **kv,blob:** Confine data-plane access to the caller's projects
- **auth:** Select jsonwebtoken crypto provider ([#497](https://github.com/gotempsh/temps/issues/497))

### Performance

- **otel:** Make trace-summaries scale with the query window

### Styling

- **kv,blob:** Apply rustfmt

### Testing

- **audit:** Add authorization tests for audit-log endpoints
- **auth:** Relocate audit:read matrix test to avoid overlap with #350

## [0.1.0-nightly.20260731.42fe6068] - 2026-07-30

### Fixed

- **ci:** Pin privileged workflow actions ([#473](https://github.com/gotempsh/temps/issues/473))
- **auth:** Enforce privilege ceiling on key rotation ([#472](https://github.com/gotempsh/temps/issues/472))
- **security:** Audit explicit credential reveals ([#459](https://github.com/gotempsh/temps/issues/459))
- **providers:** Sort slow queries server-side instead of client-side ([#480](https://github.com/gotempsh/temps/issues/480))
- **projects:** Resolve nixpacks variants to base nixpacks preset
- **presets:** Generalize nixpacks provider selection
- **web:** Align preset config request types
- **deployments:** Persist terminal status for jobs that never run
- **screenshots:** Redact credentials from remote provider health-check errors
- **core:** Cancel jobs left pending when a workflow aborts mid-batch

### Miscellaneous

- **web:** Regenerate Nixpacks preset types

### Testing

- **presets:** Remove ineffective struct update

## [0.1.0-nightly.20260730.e145a940] - 2026-07-30

### Fixed

- **release:** Install protoc for sandbox helpers ([#479](https://github.com/gotempsh/temps/issues/479))

## [0.1.0-nightly.20260730.ed853d3a] - 2026-07-30

### Fixed

- **release:** Use git cli for dependencies ([#476](https://github.com/gotempsh/temps/issues/476))

## [0.1.0-nightly.20260729.347ef444] - 2026-07-29

### Fixed

- **release:** Identify repository for nightly dispatch ([#475](https://github.com/gotempsh/temps/issues/475))

## [0.1.0-nightly.20260729.bd382d68] - 2026-07-29

### Added

- **skills:** Add temps-best-practices skill ([#453](https://github.com/gotempsh/temps/issues/453))
- **providers:** Pg_stat_statements slow-query monitoring + service log filtering ([#460](https://github.com/gotempsh/temps/issues/460))

### Documentation

- **skills:** Add runtime and telemetry guardrails ([#469](https://github.com/gotempsh/temps/issues/469))

### Fixed

- **otel:** Warn AI chat that only duration_ms is milliseconds, not raw attributes ([#461](https://github.com/gotempsh/temps/issues/461))
- **skills:** Harden skill security and add full audit gate ([#462](https://github.com/gotempsh/temps/issues/462))
- **web:** Duplicate day-label tooltip + cli: --connection flag for projects git ([#456](https://github.com/gotempsh/temps/issues/456))
- **deployments:** Scope job metadata to projects ([#465](https://github.com/gotempsh/temps/issues/465))
- **web:** Portal DropdownMenuSubContent to escape clipped parent ([#467](https://github.com/gotempsh/temps/issues/467))
- **release:** Dispatch builds for nightly tags ([#470](https://github.com/gotempsh/temps/issues/470))
- **release:** Recover failed nightly dispatches ([#471](https://github.com/gotempsh/temps/issues/471))

### Performance

- **build:** Isolate BuildKit cache mounts ([#463](https://github.com/gotempsh/temps/issues/463))

## [0.1.0-nightly.20260729.419505e0] - 2026-07-28

### Added

- **monitoring:** Metric alerts as config-as-code in .temps.yaml ([#454](https://github.com/gotempsh/temps/issues/454))
- **cli:** Add `temps projects secrets` subcommand ([#458](https://github.com/gotempsh/temps/issues/458))

### Documentation

- **readme:** Move why-statement above the fold, add stop-paying banner ([#457](https://github.com/gotempsh/temps/issues/457))

### Fixed

- **cli:** Add per-command --context flag to avoid silent wrong-server runs ([#455](https://github.com/gotempsh/temps/issues/455))

## [0.1.0-nightly.20260727.32b7f235] - 2026-07-27

### Added

- **agents:** Configurable AI autofix runs with per-provider turn limits ([#435](https://github.com/gotempsh/temps/issues/435))
- **email:** Choose provider type on a dedicated page before configuring ([#438](https://github.com/gotempsh/temps/issues/438))
- **web:** Onboard users into AI autofix from error tracking ([#439](https://github.com/gotempsh/temps/issues/439))
- **sandbox:** Track agent-run sandboxes as first-class sandbox items ([#436](https://github.com/gotempsh/temps/issues/436))
- **import:** Kubernetes, Coolify, Dokploy, CapRover, Portainer, and Kamal importers with deploy-and-verify ([#441](https://github.com/gotempsh/temps/issues/441))

### CI

- **release:** Add nightly build workflow ([#452](https://github.com/gotempsh/temps/issues/452))

### Documentation

- README overhaul, unified project creation, provider brand logos ([#446](https://github.com/gotempsh/temps/issues/446))

### Fixed

- **web:** Stop analytics/errors setup-redirect from breaking project tour ([#434](https://github.com/gotempsh/temps/issues/434))
- **agents:** Stop dumping raw CLI JSONL as autofix error messages ([#437](https://github.com/gotempsh/temps/issues/437))
- **web:** Clear all high-severity bun audit advisories ([#443](https://github.com/gotempsh/temps/issues/443))
- **deps:** Patch Dependabot advisories in next, setuptools and serde_with ([#442](https://github.com/gotempsh/temps/issues/442))
- **import:** Trim session credential lifetime + drop stale web cast ([#448](https://github.com/gotempsh/temps/issues/448))
- **monitoring:** Close race that fires false ContainerCrash alerts on deploy ([#451](https://github.com/gotempsh/temps/issues/451))

### Performance

- **observe:** Stop proxy-log and span listings scanning the whole retention window ([#447](https://github.com/gotempsh/temps/issues/447))

## [0.1.0-beta.54] - 2026-07-24

### Added

- **otel:** Store cross-project trace refs in ClickHouse when enabled ([#429](https://github.com/gotempsh/temps/issues/429))

### Fixed

- **web:** Bump rrweb-player to 2.1.1 (broken 2.1.0 dist → blank replays) ([#430](https://github.com/gotempsh/temps/issues/430))

## [0.1.0-beta.53] - 2026-07-23

### Added

- **error-tracking:** Source context for native stack traces (Go/Rust/all languages) ([#419](https://github.com/gotempsh/temps/issues/419))
- **web:** Add project onboarding tour
- **web:** Add subtle "Take a tour" relaunch on project overview
- **error-tracking:** Default source capture to the Docker build context + configurable root ([#423](https://github.com/gotempsh/temps/issues/423))

### Fixed

- **web:** Ignore spurious empty-string onValueChange from Radix Select ([#424](https://github.com/gotempsh/temps/issues/424))
- **deployments:** Stop infinite reconnect loop on container logs ([#425](https://github.com/gotempsh/temps/issues/425))
- **observability:** Read Observe feed through ClickHouse-aware storage backends ([#426](https://github.com/gotempsh/temps/issues/426))

### Miscellaneous

- **templates:** Update observability-starter entry for Cadence demo

## [0.1.0-beta.52] - 2026-07-22

### Added

- **containers:** Show metrics history in container detail ([#415](https://github.com/gotempsh/temps/issues/415))
- **config:** Expose sandbox backend selection in settings API and UI ([#414](https://github.com/gotempsh/temps/issues/414))

### Fixed

- **web:** Override brace-expansion and js-yaml to patched versions ([#409](https://github.com/gotempsh/temps/issues/409))
- **observability:** Prevent ClickHouse Array(Nothing) decode failures ([#408](https://github.com/gotempsh/temps/issues/408))
- **clickhouse:** Align query result integer types ([#416](https://github.com/gotempsh/temps/issues/416))

## [0.1.0-beta.51] - 2026-07-21

### Added

- **deployments:** Preview commits before deployment ([#379](https://github.com/gotempsh/temps/issues/379))
- **email:** Dedup shared email UI helpers, working event filters, per-domain delivery stats ([#307](https://github.com/gotempsh/temps/issues/307))
- **deployments:** Preview tag commits before deployment ([#383](https://github.com/gotempsh/temps/issues/383))
- **email:** Add provider detail page with domains and delivery tracking setup ([#382](https://github.com/gotempsh/temps/issues/382))
- **telemetry:** Add deploy_cancelled event ([#385](https://github.com/gotempsh/temps/issues/385))
- **web:** Add metrics storage backend selector to monitoring settings ([#399](https://github.com/gotempsh/temps/issues/399))
- **sandbox:** Firecracker microVM backend alongside Docker (ADR-029)

### Fixed

- **cli:** Type email command output and sanitize rendered bodies ([#306](https://github.com/gotempsh/temps/issues/306))
- **email:** Secure SES SNS event processing with one-click tracking setup ([#297](https://github.com/gotempsh/temps/issues/297))
- **email:** Stop leaking suppressed recipient addresses via logs/error_message ([#380](https://github.com/gotempsh/temps/issues/380))
- **web:** Make DNS records table horizontally scroll on mobile ([#381](https://github.com/gotempsh/temps/issues/381))
- **email:** Correct SES IAM action namespace from sesv2: to ses: ([#384](https://github.com/gotempsh/temps/issues/384))
- **analytics:** Derive bounce/entry/exit from session pageviews at query time ([#398](https://github.com/gotempsh/temps/issues/398))
- **sandbox:** Address PR #400 review — CI, OpenAPI/SDK, guard, security
- **sandbox:** Satisfy clippy + vercel-compat guardrail (PR #400 CI)
- **cli:** Update sandbox_url tests to canonical plural route
- **proxy:** Resolve request-log detail by request_id across storage backends ([#402](https://github.com/gotempsh/temps/issues/402))
- **audit:** Keep audit history when a user account is deleted ([#386](https://github.com/gotempsh/temps/issues/386))

### Miscellaneous

- **auth:** Remove magic-link login ([#375](https://github.com/gotempsh/temps/issues/375))

## [0.1.0-beta.50] - 2026-07-17

### Added

- **backup:** Dump only critical table data in control-plane backups ([#367](https://github.com/gotempsh/temps/issues/367))
- **proxy:** Trust CF-Connecting-IP from verified Cloudflare egress ranges ([#368](https://github.com/gotempsh/temps/issues/368))
- **monitoring:** Track container CPU/memory for external services ([#371](https://github.com/gotempsh/temps/issues/371))
- **observability:** Compress immutable telemetry after 24h ([#370](https://github.com/gotempsh/temps/issues/370))
- **telemetry:** Aggregated anonymous error_summary event ([#373](https://github.com/gotempsh/temps/issues/373))
- **telemetry-api:** Accept error_summary event ([#374](https://github.com/gotempsh/temps/issues/374))
- **auth:** Let deployment tokens call the AI gateway (ai_gateway:execute) ([#377](https://github.com/gotempsh/temps/issues/377))

### CI

- **compose-security:** Cache Docker toolchain layers and prebuild with fast profile ([#362](https://github.com/gotempsh/temps/issues/362))

### Documentation

- **skill:** Add remote-over-SSH install method to temps-platform-setup ([#366](https://github.com/gotempsh/temps/issues/366))

### Fixed

- **core:** Resolve request IP trust-awarely for audit/logging ([#363](https://github.com/gotempsh/temps/issues/363))
- **skill:** Remove piped shell install and explicit credential paths from docs ([#365](https://github.com/gotempsh/temps/issues/365))
- **deployments:** Emit deploy_succeeded telemetry on the real success path ([#372](https://github.com/gotempsh/temps/issues/372))
- **webhooks:** Pin delivery to validated IP to close DNS-rebinding SSRF ([#332](https://github.com/gotempsh/temps/issues/332))
- **observability:** Read active Timescale policies ([#378](https://github.com/gotempsh/temps/issues/378))

## [0.1.0-beta.49] - 2026-07-16

### Added

- **analytics:** Add insights panel with stat and AI insights
- **analytics:** Put insights behind a compact toggle button
- **ai-chat:** Default-on read-only chat; route analytics AI insights through project chat
- **analytics:** Add raw event entries drill-down with JSON props view ([#359](https://github.com/gotempsh/temps/issues/359))
- **settings:** Show web-console banner when a newer release is available ([#353](https://github.com/gotempsh/temps/issues/353))

### Fixed

- **analytics:** Harden AI insights prompt + warn on partial AI disable
- **proxy:** Harden preview cookie session handling ([#361](https://github.com/gotempsh/temps/issues/361))

### Miscellaneous

- **web:** Replace rocketship logo assets with the t brand mark ([#358](https://github.com/gotempsh/temps/issues/358))

### Performance

- **metrics:** Time-bound external-service latest-metric queries ([#364](https://github.com/gotempsh/temps/issues/364))

## [0.1.0-beta.48] - 2026-07-15

### Added

- **skills:** Add estimate-temps-savings skill ([#357](https://github.com/gotempsh/temps/issues/357))

### Fixed

- **backup:** Stop stranding failed uploads that block retention cleanup ([#356](https://github.com/gotempsh/temps/issues/356))

### Miscellaneous

- **mcp:** Remove @temps-sdk/mcp package ([#355](https://github.com/gotempsh/temps/issues/355)) [**BREAKING**]

## [0.1.0-beta.47] - 2026-07-15

### Added

- **error-tracking:** Deep-link error alert emails, verify Slack stays HTML-free ([#308](https://github.com/gotempsh/temps/issues/308))
- **analytics:** Add daily returning visitor metric ([#346](https://github.com/gotempsh/temps/issues/346))
- **backups:** Add retention cleanup and manual deletion ([#336](https://github.com/gotempsh/temps/issues/336))
- **monitoring:** Monitor all mounted disks for disk-space alerts ([#349](https://github.com/gotempsh/temps/issues/349))
- **settings:** Add flat public hostname strategy ([#146](https://github.com/gotempsh/temps/issues/146))

### CI

- Remove dependabot auto-merge workflow ([#345](https://github.com/gotempsh/temps/issues/345))

### Fixed

- **metrics:** Don't UNION checkpoint queries across pg_stat_checkpointer/bgwriter ([#290](https://github.com/gotempsh/temps/issues/290))
- **metrics:** Use mongodb's re-exported bson instead of a standalone dep ([#291](https://github.com/gotempsh/temps/issues/291))
- **migrations:** Skip empty visitor deduplication rewrites ([#294](https://github.com/gotempsh/temps/issues/294))
- **deployer:** Harden compose deployments
- **deployer:** Close compose security policy bypasses from review
- **deployer:** Reject interpolation bypass and confine compose paths
- **deployer:** Prevent compose conflict container deletion
- **deployer:** Fold inline compose override allow-list into host-escape hardening
- **deployer:** Close compose host-escape bypasses found in review
- **core:** Update node pki for rcgen 0.14
- **deps:** Resolve RustSec advisory updates
- **proxy:** Update test cert generation for rcgen 0.14
- **deployer:** Close volumes_from and absolute bind-mount host-escape bypasses
- **deployer:** Close remaining compose host-escape gaps from review
- **deployer:** Close compose symlink escape paths
- **deployments:** Cap hosted website memory by default ([#164](https://github.com/gotempsh/temps/issues/164))
- **deps:** Unbreak build — pin aws-smithy (schema 0.1.0) and revert sqlx to 0.8 ([#333](https://github.com/gotempsh/temps/issues/333))
- **auth:** Prevent MFA challenge session from authenticating real requests ([#326](https://github.com/gotempsh/temps/issues/326))
- **web:** Satisfy ESLint 10 assignment rules
- **git:** Constant-time comparison for GitHub webhook HMAC signatures ([#334](https://github.com/gotempsh/temps/issues/334))
- **auth:** Close assign_role privilege-escalation (admin gate + single target) ([#324](https://github.com/gotempsh/temps/issues/324))
- **webhooks:** Close retry_delivery cross-tenant IDOR ([#329](https://github.com/gotempsh/temps/issues/329))
- **otel:** Make otel_spans compression effective by dropping trace_id from segmentby ([#348](https://github.com/gotempsh/temps/issues/348))
- **compose:** Require DB/Redis secrets, stop publishing internal ports on 0.0.0.0 ([#330](https://github.com/gotempsh/temps/issues/330))
- **git:** Bind webhook tokens to projects ([#335](https://github.com/gotempsh/temps/issues/335))
- **query-postgres:** Block function-call SQLi bypass in data-explorer WHERE clauses ([#328](https://github.com/gotempsh/temps/issues/328))

### Miscellaneous

- **deployer:** Narrow compose hardening scope
- **deps-dev:** Bump @eslint/js from 9.37.0 to 10.0.1 in /web

### Performance

- **web:** Lazy-load and paginate proxy traffic-by-project table ([#292](https://github.com/gotempsh/temps/issues/292))

### Styling

- **deployer:** Cargo fmt compose policy

### Testing

- **metrics:** Regression coverage for pg_stat_checkpointer/bgwriter query ([#293](https://github.com/gotempsh/temps/issues/293))

## [0.1.0-beta.46] - 2026-07-12

### Added

- **otel,proxy:** Parameterize ClickHouse retention via per-row retention_days
- **web:** Add per-dimension web vitals breakdown to speed insights
- **analytics-performance:** Geo breakdowns and segment filters for speed metrics
- **web:** World map of web vitals by country on the speed page
- **analytics-performance:** Read-time bot filtering for speed metrics

### Documentation

- **agents:** Document Docker safety constraints ([#232](https://github.com/gotempsh/temps/issues/232))
- **retention:** Remove EE mentions from OSS comments

### Fixed

- **web:** Adapt chart and replay types to recharts 3 and rrweb-player 2 ([#277](https://github.com/gotempsh/temps/issues/277))
- **otel,proxy:** Register RetentionResolver via the service registry
- **otel,proxy:** Defer RetentionResolver lookup past plugin registration order
- **analytics:** Exclude bots and datacenter IPs from live-visitors ([#281](https://github.com/gotempsh/temps/issues/281))
- **proxy:** Thread a shared RetentionResolver into the live Pingora proxy
- **retention:** Write-once guard on RetentionResolverSlot + guardrail docs
- **analytics-performance:** Group page metrics on performance_metrics columns
- **react-analytics:** Send pathname field the speed ingest endpoint expects
- **security:** Close three unauthenticated-access CRITICALs ([#288](https://github.com/gotempsh/temps/issues/288))
- **providers:** Reject client-supplied internal-only fields on external-service create ([#287](https://github.com/gotempsh/temps/issues/287))

### Miscellaneous

- **deps-dev:** Bump eslint from 9.37.0 to 10.6.0 in /web ([#213](https://github.com/gotempsh/temps/issues/213))
- **deps-dev:** Bump @rsbuild/core from 1.5.1 to 2.1.4 in /web ([#209](https://github.com/gotempsh/temps/issues/209))
- **deps-dev:** Bump @rsbuild/plugin-react from 1.4.0 to 2.1.0 in /web ([#217](https://github.com/gotempsh/temps/issues/217))
- **sdk:** Bump analytics-browser, kv, blob, node-sdk for npm publish ([#286](https://github.com/gotempsh/temps/issues/286))

### Performance

- **proxy:** Serve proxy-log stats from a 1-minute continuous aggregate ([#278](https://github.com/gotempsh/temps/issues/278))
- **otel:** Make storage quota opt-in, disabled by default ([#283](https://github.com/gotempsh/temps/issues/283))

## [0.1.0-beta.45] - 2026-07-11

### Added

- **web:** Add Let's Encrypt contact email to Settings page ([#273](https://github.com/gotempsh/temps/issues/273))
- **dns:** Add Pebble challtestsrv-backed DNS provider for local testing ([#275](https://github.com/gotempsh/temps/issues/275))

### CI

- **release:** Cut macOS/web build time in release pipeline ([#271](https://github.com/gotempsh/temps/issues/271))

### Fixed

- **proxy:** Isolate preview connection-pool by sandbox target ([#274](https://github.com/gotempsh/temps/issues/274))
- **proxy:** Track real body bytes for request/response bandwidth ([#276](https://github.com/gotempsh/temps/issues/276))

## [0.1.0-beta.44] - 2026-07-10

### Fixed

- **domains:** Wire config/DNS services into TlsService for auto-renewal ([#270](https://github.com/gotempsh/temps/issues/270))

## [0.1.0-beta.43] - 2026-07-10

### Added

- **analytics-sdk:** Exclude sensitive paths from session replay by default
- **auth:** Audit concurrent sessions and allow requiring MFA for admins ([#189](https://github.com/gotempsh/temps/issues/189))
- **deployments:** Add generic DeploymentGate extension point ([#229](https://github.com/gotempsh/temps/issues/229))
- **providers:** Add managed S3 backend protocol + minIO support ([#144](https://github.com/gotempsh/temps/issues/144))
- **auth:** Add platform-admin role, key/token rotation, and MCP kill-switch ([#191](https://github.com/gotempsh/temps/issues/191))
- **deployments:** Add SecretsManagerResolver OSS extension seam (ADR 0009 §1-2) ([#252](https://github.com/gotempsh/temps/issues/252))
- **proxy:** Hot-path metrics with node dashboard and default alerts ([#258](https://github.com/gotempsh/temps/issues/258))
- **auth:** Add ProjectAccessChecker trait and project_access_guard! macro (ADR 028 Phase A) ([#260](https://github.com/gotempsh/temps/issues/260))
- **ai-chat:** Allow AI write tool to provision and link external services ([#256](https://github.com/gotempsh/temps/issues/256))
- **auth:** Wire project_access_guard! into all project-scoped handlers (ADR 028 Phase B) ([#261](https://github.com/gotempsh/temps/issues/261))
- **ai-tools:** Expose deployment and node metrics to AI read allowlist ([#265](https://github.com/gotempsh/temps/issues/265))
- **auth:** Project-scoped permission narrowing (project_permission_guard!) ([#268](https://github.com/gotempsh/temps/issues/268))

### CI

- Add dependency and container scanning for Temps' own supply chain ([#187](https://github.com/gotempsh/temps/issues/187))
- Auto-merge dependabot patch/minor PRs once CI passes ([#228](https://github.com/gotempsh/temps/issues/228))
- Reduce rust-cache eviction churn and stop starving PRs of it ([#248](https://github.com/gotempsh/temps/issues/248))
- Fix unit-tests build taking 22m+ due to feature-flag cache mismatch
- Build test binaries once via cargo-nextest, share across all jobs ([#253](https://github.com/gotempsh/temps/issues/253))

### Documentation

- Forbid environment variables for runtime configuration
- Require new features to scale on small resources ([#257](https://github.com/gotempsh/temps/issues/257))
- **adr:** Project-scoped RBAC enforcement (ADR 028) ([#259](https://github.com/gotempsh/temps/issues/259))

### Fixed

- **ci:** Make rust-tests reliably fail on test failures
- **providers:** Bind provisioned service ports to 127.0.0.1 instead of 0.0.0.0 ([#190](https://github.com/gotempsh/temps/issues/190))
- **ci:** Serialize docker-deployments integration tests to stop Postgres deadlocks ([#195](https://github.com/gotempsh/temps/issues/195))
- **test-utils:** Disable TimescaleDB background workers in TestDatabase ([#196](https://github.com/gotempsh/temps/issues/196))
- **ci:** Disable TimescaleDB background workers in integration-tests services container ([#197](https://github.com/gotempsh/temps/issues/197))
- **web:** Resolve gitlab vs github commit links and trace UI polish ([#231](https://github.com/gotempsh/temps/issues/231))
- **web:** Add web TypeScript check to CI and fix all errors ([#233](https://github.com/gotempsh/temps/issues/233))
- Resolve stacked dependency-bump compilation breaks on main ([#247](https://github.com/gotempsh/temps/issues/247))
- Repair rcgen 0.14 API breaks on main ([#250](https://github.com/gotempsh/temps/issues/250))
- **providers:** Persist actual bound port after container creation retry ([#254](https://github.com/gotempsh/temps/issues/254))
- **auth:** Enforce privilege ceiling on custom API key permissions ([#267](https://github.com/gotempsh/temps/issues/267))
- **analytics:** Resolve visitor_id on lookup miss instead of dropping it ([#269](https://github.com/gotempsh/temps/issues/269))

### Miscellaneous

- **scripts:** Add local testcontainer orphan-cleanup script ([#246](https://github.com/gotempsh/temps/issues/246))

### Performance

- **proxy:** Remove per-request database dependencies from the hot path ([#230](https://github.com/gotempsh/temps/issues/230))
- **otel:** Cache storage quota checks to cut ingest DB load ([#262](https://github.com/gotempsh/temps/issues/262))
- **auth:** Throttle last_used_at writes on every authenticated request ([#264](https://github.com/gotempsh/temps/issues/264))
- Bound proxy-log and monitor-health hypertable lookups by time ([#266](https://github.com/gotempsh/temps/issues/266))

### Security

- Gate unauthenticated admin endpoints + expand AI chat allowlist ([#249](https://github.com/gotempsh/temps/issues/249))

## [0.1.0-beta.42] - 2026-07-04

### Added

- **web:** Surface AI invocations inline in trace views ([#185](https://github.com/gotempsh/temps/issues/185))
- **otel:** Label filters and per-series dynamic alerting for metric alerts
- **otel:** Cross-project trace linking (ADR-027) ([#186](https://github.com/gotempsh/temps/issues/186))

### Fixed

- **analytics:** Exclude zero-visitor groups from property breakdown
- **analytics:** Exclude zero-visitor groups from ClickHouse breakdown
- **domains:** Stop ACME TXT records from stacking across renewals ([#182](https://github.com/gotempsh/temps/issues/182))
- **analytics:** Stop fabricating +/-100% dashboard trend when there's no baseline
- **providers:** Harden postgres major upgrades ([#151](https://github.com/gotempsh/temps/issues/151))
- **deployer:** Make cluster DNS injection opt-in (experimental beta) ([#194](https://github.com/gotempsh/temps/issues/194))

## [0.1.0-beta.41] - 2026-07-02

### CI

- **changelog:** Skip preview comment on fork PRs

### Fixed

- **deployer:** Stop temps-dns-resolver being a container DNS SPOF

### Miscellaneous

- **changelog:** Generate CHANGELOG.md with git-cliff instead of hand-editing

## [0.1.0-beta.40] - 2026-07-01

### Added

- **notifications:** Add Cloudflare Email Sending provider ([#160](https://github.com/gotempsh/temps/issues/160))
- **otel:** ClickHouse-first OTEL metrics storage with full-fidelity decode
- **otel:** Temporality-aware rate() and histogram-summary aggregation
- **web:** Wire metrics explorer to Phase C API + all-metrics overview
- **otel:** Metric dashboards v1 (sections + metric tiles)
- **web:** Unify metrics + dashboards into one Metrics surface
- **otel:** Metric alert rules v1 (threshold alerts + evaluator)
- **otel:** V1 metric anomaly detection (robust seasonal MAD band)
- **web:** Make the anomaly alert form legible
- **otel:** Anomaly backtest — "would this have fired?"
- **web:** Shade the anomaly band on the metrics explorer
- **web:** Datadog-style "what's wrong" surface for OTEL metrics
- **web:** Deploy markers on the metrics explorer chart (Tier 2)
- **web:** Cross-signal "what changed" strip on the metrics drill-in (Tier 2)
- **web:** Datadog-style firing status on metric dashboards
- **web:** Show the anomaly band + breach on the metric chart
- **web:** Show the anomaly band on standalone metric tiles
- **web:** Custom date-range filter in the metrics explorer
- **otel:** Embed a Datadog-style chart in metric-alert emails
- **otel:** Add x-axis time ticks to alert email chart
- **otel:** Humanize metric alert notification text (ADR-021 Tier 1)
- **ai:** General AiService foundation for typed/structured output (ADR-022)
- **ai:** Add multi-turn streaming (chat_stream) to AiService (ADR-023)
- **ai:** Ai_conversations + ai_messages schema + project toggle (ADR-023)
- **ai:** Temps-ai-chat crate — conversation service + provider trait (ADR-023)
- **ai:** Deployment debug-chat provider + SSE endpoints + plugin (ADR-023)
- **projects:** Expose ai_alert_summaries_enabled + ai_debug_chat_enabled in project API (ADR-021/023)
- **web:** AI Assistance settings card — debug chat + alert summary toggles (ADR-021/023)
- **web:** DeploymentDebugChat — streaming AI debug chat on failed deployments (ADR-023)
- **ai:** Log-tail enrichment + alert investigation provider (ADR-023)
- **web:** Generalize DebugChat + mount on alert form (ADR-023)
- **ai:** Agentic repo tool-calling, live context refresh & cross-project chat list (ADR-023)
- **web:** AI Providers settings + persistent cross-project assistant dock (ADR-022/023)
- **web:** Make the AI assistant button global in the top bar (ADR-023)
- **ai:** Stream tool calls/results to the chat UI + project favicons in the switcher (ADR-023)
- **ai:** Persist tool calls for reload + chat timestamps + icon-only back button (ADR-023)
- **observe:** Show only root spans in the feed and search all spans by name
- **observability:** Storage-agnostic TraceReader (temps-core trait + otel impl)
- **ai-chat:** Project-level chats, AI trace tools, and write-path security
- **otel:** Full-fidelity TimescaleDB metrics storage at ClickHouse parity
- **ai:** Streaming agentic chat + virtual CLI tool, trace UI, alarms, mobile fixes
- **multi-node:** Harden enrollment, secrets, proxy + mTLS PKI keystone
- **multi-node:** Node enrollment tokens + registration rate limiting
- **multi-node:** MTLS cert provisioning — cluster CA + CSR signing (WS-2.1)
- **multi-node:** MTLS transport — agent TLS serving + CP client (WS-2.1)
- **multi-node:** Deploy over mTLS — wire CP client into deploy path + IP SAN fix (WS-2.1)
- **multi-node:** Route node drain through the mTLS deployer factory (WS-2.1)
- **multi-node:** MTLS for remote log streaming + shared client factory (WS-2.1)
- **multi-node:** MTLS for the terminal WebSocket proxy (WS-2.1)
- **multi-node:** Reject edge-route pulls from non-active nodes (WS-3.4, netiso-6)
- **deployments:** Inject node identity env vars into every container
- **nodes:** Surface the control plane as a node (id 0) in the admin API
- **nodes:** Alert operators when a worker node goes offline
- **nodes:** Alert operators when a worker node recovers (back online)
- **nodes:** Resource + responsiveness alerts for worker nodes
- **nodes:** Configurable resource-alert thresholds; defer latency anomaly
- **logs:** Remote-node container logs in searchable history with container/node filters
- **logs:** "All containers" live mode — merge per-container streams with source
- **monitoring:** Control-plane self-metrics (CPU/mem/disk) + alerts
- **dns:** Control-plane DNS resolver for single-node service discovery (ADR-024)
- **ai-api-tools:** Add vetted write index + prepare/execute path
- **entities,migrations:** Ai_pending_actions table + per-project write toggle
- **ai-chat:** Propose-then-confirm write actions (service+endpoints+tool)
- **serve:** Wire AI write tool with curated mutation allowlist
- **web,projects:** Confirm/reject card + per-project write-actions toggle
- **web:** Enable read-only AI chat inline from the chat surfaces
- **web:** Enable AI write actions from the chat (confirm-gated)
- **web:** Label temps_write tool cards with the command, like temps
- **web:** Show the redacted request params on the proposed-action card
- **git:** Add list_directory to GitProviderService (GitHub + GitLab)
- **ai-chat:** General repo-explore tools in every chat (git-connected)
- **ai:** AI write tools (propose-then-confirm) + chained plans, manual deploys, deploy fixes
- **projects:** Add project source-type change endpoint
- **web:** Deployment source card and docker/static deploy UI
- **git:** Add Gitea, Bitbucket, and Generic git providers ([#177](https://github.com/gotempsh/temps/issues/177))

### Documentation

- Add local build prerequisites ([#148](https://github.com/gotempsh/temps/issues/148))
- **changelog:** Add Unreleased entry for OpenTelemetry metrics ([#158](https://github.com/gotempsh/temps/issues/158))
- **adr:** ADR-021 humanized alert notification text
- **adr:** ADR-022 general AI foundation for typed/structured output
- **adr:** ADR-023 persistent AI debugging conversations
- **changelog:** Add full-fidelity TimescaleDB metrics entry ([#173](https://github.com/gotempsh/temps/issues/173))
- **adr:** Add ADR-020 multi-node deployment hardening + audit
- **adr-020:** Reassess WS-3 against the Kubernetes routing model
- **changelog:** Multi-node hardening + worker monitoring
- **examples:** Add echo-server example + dev-cluster multi-node deploy
- **deployer:** Note the deliberate get_service for CP DNS bootstrap
- **changelog:** Add OTel project-scope + AI tool-discovery security entries
- **changelog:** Document streaming AI chat + temps API tool, trace detail, OTLP log fixes
- **changelog:** AI propose-then-confirm write actions

### Fixed

- **web:** Wrap long values in trace span detail panel ([#159](https://github.com/gotempsh/temps/issues/159))
- **auth:** Constrain deployment token permissions
- **auth:** Preserve email access and enforce project scope for deployment tokens
- **otel:** Temporality-correct histogram_summary (no cumulative double-count)
- **web:** Edit form dropped changed Select values (aggregation, detection)
- **web:** Keep the project header on one line for long titles
- **otel:** Qualify service_name in trace WHERE to dodge alias-shadow
- **ai:** Default model fallback so a provider key alone enables AI (ADR-022)
- **ai:** PR #158 review — tenant-scope guards, traversal/UTF-8 hardening, typing-bubble + tool-loop tests (ADR-023)
- **ai:** PR #158 review — medium findings (ADR-022/023)
- **cli:** Cancel the running migration backend on Ctrl+C
- **otel:** Scope-guard metric label endpoints + 400 on bad label key
- **multi-node:** Address security-auditor findings (enrollment/mTLS hardening)
- **logs:** History filter dropdowns list all sources, not just the current selection
- **logs:** Chronologically interleave the "All containers" live stream
- **mtls:** Server-authoritative cert SANs + close #162 review gaps
- **security:** Patch reachable dependency advisories (postgres, quic, aiohttp)
- **notifications:** Disambiguate update_email_provider operationId ([#163](https://github.com/gotempsh/temps/issues/163))
- **env-vars,dns:** 409 on duplicate env var + idempotent DNS publish
- **dns:** Forward NODATA as NOERROR, not NXDOMAIN
- **dns:** IPv4-only gateway selection + feeder update-propagation test
- **otel,ai:** Scope-guard OTel query handlers, permission-aware AI discovery, clippy
- **edge:** Stop forwarding edge token on asset misses ([#169](https://github.com/gotempsh/temps/issues/169))
- **deployments:** Enforce container exec scope ([#166](https://github.com/gotempsh/temps/issues/166))
- **ai-chat,serve:** Harden propose-then-confirm per security review
- **ai-chat:** Write-actions-on must not block the read-only chat
- **ai-write:** Add redeploy op, human-readable proposals, better op guidance
- **ai-write:** Show the full write-op catalog so redeploy is discoverable
- **ai-chat,web:** Sidebar visibility + redeploy env + write-op diagnostic
- **web:** Mobile horizontal scroll in deployment job logs

### Miscellaneous

- **web:** Regenerate SDK for AI chat routes + project AI toggles (ADR-023)
- **dev-cluster:** Add cluster-formation e2e trace script
- **examples:** Add OpenTelemetry + error-tracking multi-node demo
- **web:** Regenerate OpenAPI SDK after merging main
- **web/sdk:** Add ai_write_actions_enabled to project SDK types
- **web/sdk:** Regenerate OpenAPI client for AI write actions
- **web:** Regenerate OpenAPI SDK

### Performance

- **proxy:** Cache IP block-list and geolocation to cut per-request DB load ([#174](https://github.com/gotempsh/temps/issues/174))

### Refactor

- **otel:** Forward-compatible detector schema for metric alert rules
- **web:** Group project nav into OpenTelemetry + Monitoring
- **ai:** Extract reusable temps-ai crate with schemas + diagnostics

### Styling

- **analytics-events:** Cargo fmt
- **web:** Full-width metrics + dashboards pages

### Testing

- **otel:** Live-ClickHouse round-trip for exp-histogram/summary/exemplar columns
- **multi-node:** Real container deploy over mTLS (gated live test)
- **dev-cluster:** From-scratch multi-node e2e harness (mTLS join + deploy)
- **providers:** Isolate lifecycle tests' ports and container names ([#171](https://github.com/gotempsh/temps/issues/171))

## [0.1.0-beta.39] - 2026-06-25

### Added

- **temps-cli:** Per-instance default project, static deploy in `up`, full build logs ([#154](https://github.com/gotempsh/temps/issues/154))
- **deployments:** Rebuild from source on rollback for git projects ([#155](https://github.com/gotempsh/temps/issues/155))
- **templates:** One-click demo deploy for activation (no Git account) ([#157](https://github.com/gotempsh/temps/issues/157))

### Documentation

- **agents:** Document changelog CI gate and skip-changelog label ([#156](https://github.com/gotempsh/temps/issues/156))

### Fixed

- **ci:** Serialize heavy provider backup tests + dedupe port selection ([#152](https://github.com/gotempsh/temps/issues/152))

## [0.1.0-beta.38] - 2026-06-23

### Added

- **tls:** On-demand certs for the console host + sslip.io auto-enable ([#147](https://github.com/gotempsh/temps/issues/147))

## [0.1.0-beta.35] - 2026-06-19

### Added

- **tls:** On-demand HTTP-01 issuance (ADR-018) + renewal-safety fixes ([#137](https://github.com/gotempsh/temps/issues/137))
- **monitoring:** Enable monitoring by default for new services + DNS/domain UI polish ([#142](https://github.com/gotempsh/temps/issues/142))
- **sdk:** Add @temps-sdk/api generated OpenAPI client package ([#143](https://github.com/gotempsh/temps/issues/143))

## [0.1.0-beta.34] - 2026-06-17

### Added

- Anonymous telemetry + Postgres shm_size_mb + custom health-check path ([#135](https://github.com/gotempsh/temps/issues/135))

## [0.1.0-beta.33] - 2026-06-17

### Added

- **serve:** Split proxy and console into independent processes (ADR-017 Phase 1)
- **proxy:** Wire cross-process on-demand wake into `temps proxy` (ADR-017 Phase 2)
- **serve:** Version-skew detection + `temps upgrade --split` guidance (ADR-017 Phase 3)
- **environments:** Per-environment attack_mode override

### Documentation

- **adr-017:** Design Phase 4 — zero-downtime proxy restarts via Pingora graceful upgrade

### Fixed

- **deployments:** Deploy environments uncapped by default, limits opt-in ([#132](https://github.com/gotempsh/temps/issues/132))
- **monitoring:** Fire CPU alarms relative to the container CPU limit
- **git:** SSRF guard + token-drop on GitLab archive redirects

## [0.1.0-beta.31] - 2026-06-11

### Fixed

- **proxy:** Probe app readiness (TCP) before completing on-demand wake ([#128](https://github.com/gotempsh/temps/issues/128))
- **proxy:** Filter on-demand wake/sleep by node_id + stop leaking detail in 503 bodies
- **logs:** Filter history logs by deployment id (uuid → i32) ([#131](https://github.com/gotempsh/temps/issues/131))
- **web:** Improve runtime History log filter layout and multi-day timestamps ([#130](https://github.com/gotempsh/temps/issues/130))

## [0.1.0-beta.30] - 2026-06-10

### Fixed

- **proxy:** Wake on-demand environments via in-process ForceRouteReload to fix first-request 503 ([#124](https://github.com/gotempsh/temps/issues/124))

## [0.1.0-beta.29] - 2026-06-09

### Added

- **deployments:** Capture & view logs of previous deployments + security hardening ([#123](https://github.com/gotempsh/temps/issues/123))

### Fixed

- **domains:** Show actionable card for unclassifiable ACME challenges

## [0.1.0-beta.28] - 2026-06-08

### Fixed

- **security:** Bump vitest to 4.1.0, aiohttp to 3.14.0, pingora to 0.8.1 ([#120](https://github.com/gotempsh/temps/issues/120))
- **git:** Ignore null-SHA push events to prevent failed 0000000 deployments ([#121](https://github.com/gotempsh/temps/issues/121))
- **serve:** Remove duplicate backup scheduler + feat(console-kit): headerActions slot ([#122](https://github.com/gotempsh/temps/issues/122))

## [0.1.0-beta.27] - 2026-06-05

### Added

- **onboarding:** Setup_complete flag, wizard bypass, per-domain HTTPS, activation checklist
- **onboarding:** Improve getting-started checklist UX
- **proxy:** Add AI-agent pages endpoint to proxy logs

### Documentation

- **skills:** Refresh add-custom-domain and platform-setup skills
- **changelog:** Record metrics-store mismatch fix, on-demand sandbox build, and related changes

### Fixed

- **projects:** Send null instead of empty string for git_url when using PAT provider
- **projects:** Send null instead of empty string for git_url in all create-project paths
- **settings:** Surface effective metrics store and warn on ClickHouse mismatch
- **agents:** Build sandbox image on demand only, never at startup
- **domains:** Renew HTTP-01 certificates via the order-based ACME flow
- **deployments:** Use project slug for remote deployment hostnames
- **web:** Console UI polish across palette, activity graph, log viewer, date picker
- **agents:** Use ghcr.io/gotempsh/temps-preview-gateway image
- **deployments:** Pass private registry credentials when pulling external images

### Miscellaneous

- Migrate CLI dry-run/confirm/live-progress + chart tooltip fix

## [0.1.0-beta.26] - 2026-06-03

### Added

- **analytics:** Add `temps backfill clickhouse` standalone migration subcommand ([#109](https://github.com/gotempsh/temps/issues/109))
- **metrics:** Unified database observability — service metrics, OTLP ingest, monitoring UI ([#108](https://github.com/gotempsh/temps/issues/108))
- **analytics:** AI agents overview — timeline chart, breakdown cards, status ([#113](https://github.com/gotempsh/temps/issues/113))
- **logs:** Grep -C surrounding lines in log search + trace duration sort ([#114](https://github.com/gotempsh/temps/issues/114))
- **clickhouse:** Opt-in CH telemetry backends + TimescaleDB trace summaries, backfill, migrate reporting & data-model hardening ([#116](https://github.com/gotempsh/temps/issues/116))

### Documentation

- **skills:** Fix HIGH-risk security findings in temps-platform-setup skill ([#110](https://github.com/gotempsh/temps/issues/110))

### Fixed

- **domains:** Recover stuck TLS orders and fix HTTP-01 auto-renewal ([#111](https://github.com/gotempsh/temps/issues/111))

## [0.1.0-beta.25] - 2026-05-31

### Added

- **analytics:** Per-project AI Crawler activity feed ([#107](https://github.com/gotempsh/temps/issues/107))

## [0.1.0-beta.24] - 2026-05-30

### Added

- **cli:** TEMPS_CONTEXT env override + correct api-key login server
- **analytics:** AI agent traffic analytics + proxy-log filtering
- **analytics:** Per-agent breakdown when expanding a crawled page
- **analytics:** AI agents detail redesign + CLI ai-agents commands
- **containers:** Live log levels + pause/resume and credential masking
- **migrations:** Backfill proxy_logs.bot_name with canonical AI-agent names

### Documentation

- **changelog:** Record fast LB bind + TEMPS_CONTEXT CLI changes

### Fixed

- **proxy:** Run AI-agent detection on the live proxy-log ingest path

### Performance

- **proxy:** Bind the load balancer before loading routes

### Refactor

- **migrations:** Make AI-agent bot_name backfill a manual script

## [0.1.0-beta.23] - 2026-05-29

### Added

- **auth:** Working password reset via email-only delivery ([#102](https://github.com/gotempsh/temps/issues/102))

## [0.1.0-beta.22] - 2026-05-27

### Added

- **git:** Vercel-style sticky PR/MR preview comments on deploys ([#96](https://github.com/gotempsh/temps/issues/96))
- **proxy:** Branded 404 for unknown hosts behind admin gate ([#97](https://github.com/gotempsh/temps/issues/97))
- **projects:** On-demand preview environments by default
- **deployer:** Control-plane build concurrency + per-build resource caps
- **auth:** Per-OIDC-provider trust_idp_email opt-out for email_verified gate
- **email:** Generic SMTP provider, edit endpoint, and branded health-check email ([#101](https://github.com/gotempsh/temps/issues/101))

### Documentation

- **changelog:** Record the on-demand previews / cancel comment / build limits work

### Fixed

- **security:** 0.1.0 hardening pass (29 findings, 3 rounds) ([#95](https://github.com/gotempsh/temps/issues/95))
- **security:** Close 2 remaining 0.1.0 release blockers ([#98](https://github.com/gotempsh/temps/issues/98))
- **git:** Update PR preview comment on deployment cancel
- **web:** GitLab logo + cleaner delete-provider toast

### Miscellaneous

- Bump version, serialize docker tests, dns updates

## [0.1.0-beta.21] - 2026-05-24

### Added

- **auth:** OIDC SSO + Keycloak dev tooling + workflow trigger fix ([#93](https://github.com/gotempsh/temps/issues/93))

## [0.1.0-beta.20] - 2026-05-21

### Added

- **web:** Change platform logo and favicon to the "t" lettermark
- **notifications:** Real data aggregation for weekly digest
- **email:** Native email validation, drop check-if-email-exists
- **ai-gateway:** Paginate and filter recent requests usage log

### Fixed

- **notifications:** Rebuild weekly digest email with table-based layout
- **otel:** Report the configured rate limit in RateLimitExceeded
- **deps:** Bump idna to 3.15 in Python SDK (CVE-2024-3651 bypass)
- **import-docker:** Import RestartPolicyNameEnum from bollard::models
- **deps:** Upgrade hickory-dns to 0.26.1 (DNS CVEs)
- **dns:** Migrate temps-dns-resolver test files to hickory 0.26

### Miscellaneous

- Remove unused temps-mcp crate (drops rmcp CVE)

### Refactor

- **proxy:** Remove dead RequestLogger code path

### Styling

- **deployments:** Cargo fmt routing-inputs block

### Testing

- **proxy:** Fix visitor/session tests to create real DB rows

## [0.1.0-beta.9] - 2026-05-11

### Fixed

- **sandbox:** Bake safe.directory into image and surface chown failures ([#83](https://github.com/gotempsh/temps/issues/83))
- **workspace:** Chown bind-mount work_dir to sandbox uid on the host ([#84](https://github.com/gotempsh/temps/issues/84))

## [0.1.0-beta.8] - 2026-05-10

### Fixed

- **ci:** Set automatic_deploy=true in E2E project creation ([#80](https://github.com/gotempsh/temps/issues/80))
- **deployments:** Always trigger initial deployment for new environments ([#81](https://github.com/gotempsh/temps/issues/81))
- **ci:** Wait for deployment 'completed' state before verifying app ([#82](https://github.com/gotempsh/temps/issues/82))

### Testing

- **observability:** Add hide_bots: None to EventFilters constructors in merge_integration tests ([#79](https://github.com/gotempsh/temps/issues/79))

## [0.1.0-beta.7] - 2026-05-09

### Added

- **workspace,sandbox:** V0.0.8 security audit + sandbox path centralization + CLI auto-auth ([#73](https://github.com/gotempsh/temps/issues/73))
- **cli:** Release channels for temps upgrade and install.sh ([#74](https://github.com/gotempsh/temps/issues/74))
- **providers:** Resource limits + runtime panel for external services ([#75](https://github.com/gotempsh/temps/issues/75))
- **analytics:** Runtime-toggleable ClickHouse backend ([#76](https://github.com/gotempsh/temps/issues/76))
- **workspace,deployer:** Preview password encryption + reboot-safe Docker secrets path
- **auth:** In-app password change with MFA gate and other-session revocation
- Misc improvements across git credential, sandbox, workspace, and log viewers

### Documentation

- **changelog:** Note runtime logs overhaul + storage UI polish

### Fixed

- **auth:** Preserve session cookie when clearing mfa_session after verify ([#77](https://github.com/gotempsh/temps/issues/77))
- **logs,storage:** Runtime logs overhaul (pagination, ordering, density) + storage UI polish
- **logs:** Satisfy clippy::unnecessary_sort_by on chunk + result reorder
- **web:** Projects.tsx tweak
- Respect automatic_deploy flag on git push + Observe theme tokens

## [0.1.0-beta.6] - 2026-05-03

### Added

- **cli:** Credential-based login (temps login + logout + whoami + context) ([#69](https://github.com/gotempsh/temps/issues/69))
- **dns,providers:** Internal DNS for HA databases + full cluster lifecycle (provision · scale · promote · backup · restore) ([#66](https://github.com/gotempsh/temps/issues/66))
- **observability:** Unified Observe page (cockpit + console) ([#71](https://github.com/gotempsh/temps/issues/71))

### Fixed

- **deps:** Patch 14 dependabot security advisories ([#61](https://github.com/gotempsh/temps/issues/61))
- **templates:** Image fallbacks and env var generators ([#63](https://github.com/gotempsh/temps/issues/63))
- **git:** Return 409 when GitHub repo name already exists ([#64](https://github.com/gotempsh/temps/issues/64))
- **domains:** Show renew action for ACME certificates with non-standard verification_method ([#65](https://github.com/gotempsh/temps/issues/65))
- Keep deployment log stream connected on redeploy and harden project creation ([#68](https://github.com/gotempsh/temps/issues/68))
- **operational:** Branch picker, container limits + kill reason, GitLab token refresh ([#70](https://github.com/gotempsh/temps/issues/70))
- **sandbox:** Pin image version, chown /workspace, GHCR channel split ([#72](https://github.com/gotempsh/temps/issues/72))

### Performance

- **startup:** Defer blocking subsystem init off the console boot path ([#67](https://github.com/gotempsh/temps/issues/67))

## [0.1.0-beta.3] - 2026-04-25

### Added

- **email-tracking:** Add event timeline UI and analytics dashboard ([#53](https://github.com/gotempsh/temps/issues/53))
- **agents:** AI autopilot agents framework with cron scheduling and autofixer ([#58](https://github.com/gotempsh/temps/issues/58))

### Fixed

- Configurable db pool, multi-preset detection, enter-submit wizards ([#52](https://github.com/gotempsh/temps/issues/52))
- **security:** Resolve dependency vulns and fix container exec tenant isolation ([#54](https://github.com/gotempsh/temps/issues/54))
- **email:** Fix event timeline 404 and event type mismatches ([#56](https://github.com/gotempsh/temps/issues/56))
- **email:** Email tracking fixes, analytics endpoints, SDK regen, settings validation ([#57](https://github.com/gotempsh/temps/issues/57))
- **deps:** Resolve all 18 dependabot vulnerabilities ([#60](https://github.com/gotempsh/temps/issues/60))
- **cli:** Fix project resolution in env vars subcommands and services env response ([#59](https://github.com/gotempsh/temps/issues/59))

## [0.0.8] - 2026-03-30

### Fixed

- **ci:** Add --clobber to gh release create to prevent duplicate tag failures ([#51](https://github.com/gotempsh/temps/issues/51))

## [0.0.7] - 2026-03-29

### Added

- **ci:** Add E2E deployment tests workflow
- **email:** Add open and click tracking for transactional emails

### Fixed

- Remove erroneous -- from git checkout command ([#40](https://github.com/gotempsh/temps/issues/40))
- **migrations:** Convert duplicate email_events CREATE to ALTER TABLE ([#45](https://github.com/gotempsh/temps/issues/45))
- **email:** Add deterministic ordering to get_events query ([#46](https://github.com/gotempsh/temps/issues/46))
- **presets:** Fix Next.js Docker e2e test reliability ([#47](https://github.com/gotempsh/temps/issues/47))
- **ci:** Generate self-signed certificate for E2E tests ([#48](https://github.com/gotempsh/temps/issues/48))
- **ci:** Remove duplicate trigger-pipeline call causing vite-e2e cancellation ([#49](https://github.com/gotempsh/temps/issues/49))

### Miscellaneous

- **release:** Finalize 0.0.7 changelog with missing fixes and updated date

## [0.0.7-beta1] - 2026-03-28

### Added

- **compose:** Add Docker Compose stack management
- **compose:** Add domain routing, repo-backed stacks, and replace git CLI with git2
- **compose:** Add repo compose file discovery and sync UI
- **compose:** Add port conflict validation before deploy
- **compose:** Add delete button to stack detail view
- **compose:** Add port overrides for compose stack port remapping
- **compose:** Add branch listing, fix port parser, improve create UX
- **compose:** Add docker-compose preset and service_name column (ADR-007)
- **deployer:** Add ComposeExecutor for docker compose deployments
- **compose:** Wire docker-compose into deployment pipeline
- **compose:** Wire DeployComposeJob into workflow execution service
- **compose:** Add teardown, proxy routing, and UI preset support
- **compose:** Inject Temps system env vars into all compose containers
- **compose:** Preserve volumes, add build support, expose service_name
- **containers:** Add exec and persistent terminal (xterm.js WebSocket)
- **compose:** Add user-provided compose override, PWD fix, error logging, env-file loading
- **ui:** Add public repo preset detection and compose override in git settings
- **compose:** Public repo improvements and session replay fixes
- **file-store:** Implement content-addressable storage with SHA-256
- **deploy:** Re-enable persist_static_assets with CAS backend
- **cas:** Database-backed URL→hash mapping with git-style blob sharding
- **cas:** DB-backed URL→hash mapping, git-style blob sharding, nightly GC
- **api:** Add purge asset cache endpoints
- **ui:** Add Purge Asset Cache button in environment settings
- **edge:** Add edge CDN proxy with ECIES TLS cert distribution
- **compose:** Service-specific domain routing, SQL safety, edge hardening
- **email:** Add open tracking, click tracking, and bounce/complaint webhooks
- **monitors:** Accept 404/405 as healthy and support custom check paths from .temps.yaml

### Documentation

- Update release checklist with verified items
- Update changelog with all unreleased changes
- **changelog:** Write v0.0.7 release notes

### Fixed

- **compose:** Add throwOnError to all stacks API calls
- **compose:** Throw on API errors in stacks client
- **compose:** Fix port parser trimming bug and persist tab on refresh
- **ui:** Hide application port field for docker-compose preset
- **ui:** Handle preset::path format in compose field visibility
- **ui:** Send preset_config for docker-compose in all project creation paths
- **compose:** Read compose file from workflow context, not guessed path
- **ui:** Fix port validation for docker-compose preset
- **ui:** Log form validation errors on submit failure
- **ui:** Allow NaN port value for docker-compose (field is hidden)
- **ui:** Skip setting port to 0 for docker-compose preset
- **compose:** Pass deployment_id to MarkDeploymentCompleteJob + add compose path to git settings
- **compose:** Log errors to deployment stream + fix container conflicts
- **compose:** Improve error logging and env var availability
- **git:** Fetch up to 100 branches for public repos
- **git:** Paginate branch listing for public repos (GitHub + GitLab)
- **compose:** Always use repo dir for compose commands
- **compose:** Set PWD env var for compose commands
- **ui:** Show all presets in fallback list including Docker Compose and Dockerfile
- **ui:** Show Repository link for public repos using github URL fallback
- **ui:** Public repo preset detection, compose override persistence, hide git connection for public repos
- **docs:** Use correct TimescaleDB-HA volume path for data persistence
- **compose:** Filter compose files by root dir and add screenshot job
- **ui:** Show URL input for public repo projects in Git Settings
- **ui:** Show full URL and Public badge for public repo projects
- **ui:** Fix public repo URL parsing and save in Git Settings
- **api:** Accept git_url and is_public_repo in update git settings
- **compose:** Fix preset name comparison, port parsing, and override save
- **compose:** Use host port (left side) for public port suggestions
- **git:** Use GitHub connection token for public repo API calls
- **git:** Validate GitHub token before using for public repo API calls
- **projects:** Use authenticated GitHub token in trigger-pipeline for public repos
- **deploy:** Persist_static_assets must not block mark_deployment_complete
- **deploy:** Fallback to slug-based container teardown for orphaned containers
- **deploy:** Add detailed logging for container registration in mark_complete
- **workflow:** Merge parallel job outputs instead of overwriting context
- **proxy:** Serve stale chunks via DB+CAS instead of filesystem paths
- **proxy:** Add missing temps-file-store dependency for CAS static serving
- **ui:** Show actual network throughput rate instead of cumulative total
- **ui:** Remove CPU/Memory from monitoring page, fix container name truncation
- **compose:** Inject Temps labels via compose override for log collection
- **ui:** Align container selector text to the left
- **compose:** Use correct Docker label keys for log collection
- **deploy:** Skip persist_static_assets for backend presets, accept 404 as healthy
- **deploy:** Include Dockerfile preset in persist_static_assets, skip pull for local images

### Miscellaneous

- **migrations:** Add missing asset manifest and static cache migrations
- Commit remaining unstaged changes from CAS refactor and presets

### Refactor

- **ui:** Move infrastructure pages under settings layout and sync cmd+k
- **file-store:** Manifest-based CAS instead of per-path ref files
- **ui:** Remove standalone Stacks section from sidebar and routes
- Remove standalone temps-compose crate and compose stacks

## [0.0.6] - 2026-03-19

### Added

- Embedded WireGuard, password protection, remote services & public settings ([#33](https://github.com/gotempsh/temps/issues/33))

### Documentation

- **changelog:** Finalize v0.0.6 release notes

### Fixed

- **web:** Fix funnel edit page and metrics display ([#35](https://github.com/gotempsh/temps/issues/35))

## [0.0.6-beta5] - 2026-03-11

### Added

- **web:** Add import .env file to project creation wizard
- **ai-gateway:** Add AI gateway with multi-provider support
- **ai-gateway:** Add GenAI OTel tracing, deployment promotion, on-demand environments

### Documentation

- **changelog:** Add entries for GenAI tracing, deployment promotion, on-demand environments

### Fixed

- **auth:** Update test_role_all assertion for Demo role
- **web:** Make AI Gateway page responsive for mobile devices

## [0.0.6-beta4] - 2026-03-09

### Added

- **multinode:** Add container reconciliation, scheduling config UI, and integration tests

## [0.0.6-beta3] - 2026-03-09

### Added

- External plugins, analytics overview charts, and CRON_SECRET auto-injection ([#28](https://github.com/gotempsh/temps/issues/28))
- **multinode:** Add multi-node cluster support with worker agents
- **security:** Encrypt environment variables at rest with AES-256-GCM
- **multinode:** Add container restart count and node management improvements
- **multinode:** Add alarm system, container health monitoring, and node management
- **multinode:** Teardown old containers on remote nodes and improve node management

### Documentation

- **changelog:** Add entries for multi-node cluster support

### Fixed

- **multinode:** Skip building remote env vars when no active nodes exist
- **deployments:** Prevent busy queue from starving route confirmation poll
- **deployments:** Use non-blocking advisory lock and move teardown outside lock
- **multinode:** Use Docker container names for cross-node env var rewriting
- **web:** Clean up env vars settings and add preview flag support
- **deployments:** Replace PG advisory lock with process-level mutex
- **proxy:** Add upstream connection timeouts and retry on stale connections
- **proxy:** Strip content-length from HEAD responses over HTTP/2
- **deps:** Patch critical and high severity vulnerabilities

## [0.0.6-beta.2] - 2026-03-02

### Added

- Implement job queue for route table updates and enhance deployment handling
- Add Google Indexing API plugin for Temps
- Enhance Docker deployment process with .temps.yaml support

### Documentation

- Update deployment and monitoring documentation for clarity and accuracy

## [0.0.6-beta.1] - 2026-02-28

### Added

- **domains:** Implement paginated domain listing with search functionality
- **logs:** Add structured log aggregator with Docker container log collection
- **logs:** Introduce structured log aggregator and frontend log history viewer
- **domains:** Enhance domain management with wildcard support and pagination
- Add auth rate limiting, external plugin system, docs overhaul, and settings UI

### Documentation

- Update CHANGELOG with proxy batch writer, domain pagination, and DomainSelector
- **changelog:** Add entry for Dockerfile path fix ([#26](https://github.com/gotempsh/temps/issues/26))

### Fixed

- **logs:** Use i32 project IDs, restore BuildKit build logs, and add log history UI
- **projects:** Persist Dockerfile path in project settings
- **presets:** Fix incorrect corepack command for pnpm
- **deployments:** Use Dockerfile preset in container logs WebSocket tests
- **deployments:** Use Dockerfile preset in container logs WebSocket tests

## [0.0.5] - 2026-02-27

### Added

- **mcp:** Implement MCP server with 210 tools for full platform management
- **proxy:** Add Accept: text/markdown support for AI agents
- **providers,backup,core:** Pg_dump sidecar backup, preset registry, and error hardening
- **core:** Add Next.js docs template to project templates
- **external-plugins:** Add external plugin system for standalone binary plugins ([#20](https://github.com/gotempsh/temps/issues/20))
- **otel:** Add OpenTelemetry ingest, query, and frontend traces UI ([#18](https://github.com/gotempsh/temps/issues/18))

### Documentation

- **changelog:** Add MCP server and security audit entries to changelog

### Fixed

- **cli:** Simplify closure to function reference in domain list
- **skill:** Address security audit findings in temps-cli skill
- **cli:** Escape curly braces in MDX docs output and remove credential path
- **cli:** Correct package name and command references in CLI docs
- **mcp:** Fix get_deployment_logs to fetch and parse JSONL log content
- **proxy:** Fix clippy unnecessary_literal_unwrap in markdown test and update changelog
- **proxy:** Extract <main> content before HTML-to-Markdown conversion
- **core:** Lifecycle management, bounded caches, and memory safety across 11 crates
- **environments:** Remove useless .into() conversions on chrono::Utc::now() in tests
- **backup:** Switch pg_dump to plain format to fix OOM and add error logging
- **backup:** Extend command duration for backup sidecar to prevent OOM issues
- Resolve clippy warnings for CI compliance
- **docs:** Use bash instead of sh in install script commands ([#16](https://github.com/gotempsh/temps/issues/16))
- **platform:** Proxy log retention, service UX improvements, vulnerability scanner filtering, and resource monitoring ([#14](https://github.com/gotempsh/temps/issues/14))
- **ci:** Add protoc dependency to release workflow for temps-otel build

### Miscellaneous

- **mcp:** Update version to 0.1.3 and enhance CLI help documentation
- Update Docker configurations and backup service enhancements
- Simplify Docker Compose configuration for PostgreSQL service

### Refactor

- **backup:** Update backup file extensions and improve sidecar memory management
- **backup:** Enhance backup process with direct file writing and improved error handling
- **backup:** Update backup container configuration for improved access and clarity
- **backup:** Optimize pg_dump execution to prevent memory issues

### Testing

- **proxy:** Add pipeline integration tests for markdown edge cases

## [0.0.4] - 2026-02-17

### Added

- **analytics:** Improve dashboard with drag-to-zoom, referrer tracking, and UX fixes

### Fixed

- **proxy:** Remove internal ID headers from proxy responses
- **deployments:** Add pre-flight image existence check before rollback
- **cli:** Move logs under deployments subcommand and fix log rendering
- **cli:** Populate required parameters when creating services in wizard
- **cli:** Add hardcoded fallback for service required parameters

## [0.0.3] - 2026-02-16

### CI

- **tests:** Add retry resilience for flaky test failures
- **tests:** Enhance test resilience with targeted retries and clippy advisory

### Documentation

- **README:** Add node-sdk package information to the documentation

### Fixed

- **workflows:** Enable pull request trigger for Rust tests workflow
- **deployer:** Remove unnecessary u64 cast flagged by clippy
- **providers:** Cap health check backoff and detect dead containers
- **query:** Add missing DataSource import in redis and s3 doctests

### Miscellaneous

- **dependencies:** Update various package versions and clean up Cargo.lock
- **gitignore:** Add eclipse IDE files to gitignore

### Refactor

- **providers:** Standardize PostgreSQL default to v18-alpine
- **commands:** Update data directory handling to use Path instead of PathBuf

## [0.0.2-beta9] - 2026-02-16

### Added

- **migration:** Implement migration command and related functionality
- **analytics:** Add recent activity endpoint for real-time event tracking
- **dependencies:** Update package versions and clean up Cargo.lock
- **database:** Update TimescaleDB Docker images to pg18

## [0.0.2-beta8] - 2026-02-13

### Added

- **setup:** Implement system user creation for webhook context

## [0.0.2-beta7] - 2026-02-13

### Added

- **analytics:** Add visitor journey and page flow analytics endpoints
- **analytics:** Enhance visitor analytics with EarthGlobe component and new assets
- **analytics:** Implement date filtering for visitor analytics

### Documentation

- Add Cloud ACME Certificates section for TLS provisioning

### Fixed

- **deployments:** Route docker image uploads to correct pipeline for git projects
- **deployments:** Use environment slug for manual deployment URLs

## [0.0.2-beta6] - 2026-02-12

### Documentation

- **README:** Update documentation with new links and mermaid diagrams

### Miscellaneous

- Update .gitignore to exclude certificate and key files
- Clean up localtemps app by removing unused files and directories
- Remove SKILL.md file to streamline project documentation

## [0.0.2-beta5] - 2026-02-06

### Added

- **setup:** Improve GeoLite2 database download feedback and progress indication
- **docker:** Enhance image handling and platform validation
- **analytics:** Update referrer handling for favicon display and naming
- **docker:** Improve image inspection and metadata handling

## [0.0.2-beta4] - 2026-01-29

### Added

- **skills:** Add new skills for custom domain setup, Node.js SDK integration, React analytics, session recording, and deployment management
- **events:** Enhance referrer handling in event metrics recording
- **deployments:** Add local Docker image deployment command and verification job

## [0.0.2-beta3] - 2026-01-27

### Added

- **deployments:** Streamline deployment process and enhance user experience

## [0.0.2-beta2] - 2026-01-23

### Added

- **setup:** Enhance IP address confirmation and password handling in non-interactive mode

## [0.0.2-beta1] - 2026-01-23

### Added

- **templates:** Add project templates configuration and demo mode enhancements
- **templates:** Introduce template management and TLS enhancements
- **demo:** Enhance demo mode functionality and UI components
- **workflow:** Add job configuration with custom dependencies and required flag
- **deployments:** Introduce remote deployment support and enhance project source types
- **deployments:** Add support for Docker image and static file deployments

### Miscellaneous

- **dependencies:** Update Next.js and React versions in package configuration

## [0.0.1] - 2026-01-13

### Added

- **blob, kv:** Add update functionality for Blob and KV services
- **migration:** Enhance UTM fields migration using SeaORM API
- **blob, kv:** Enhance service initialization and status handling
- **localtemps:** Initialize LocalTemps desktop app with Tauri, React, and TypeScript
- **localtemps:** Update dependencies and add DMG build script
- **localtemps:** Integrate analytics features with SeaORM and React Query
- **localtemps:** Enhance UI components and integrate new dependencies
- **analytics:** Enhance AnalyticsInspector with session replay and event categorization
- **setup:** Enhance DNS setup process with propagation verification and cleanup
- **screenshot:** Introduce NoopScreenshotProvider and enhance Chrome availability check
- **serve:** Add screenshot provider option to ServeCommand
- **temps-blob:** Migrate from MinIO to RustFS for blob storage
- **redis:** Implement pagination for Redis key listing and querying
- **demo:** Implement demo mode functionality with user role and UI adjustments

### Miscellaneous

- **redis:** Update Docker image version from 7-alpine to 8-alpine

### Refactor

- **migration:** Streamline UTM fields index creation and deletion
- **rustfs:** Implement non-blocking health check for RustFS container

## [0.0.1-beta28] - 2026-01-05

### Added

- **cli:** Enhance setup command with new options and output formats

## [0.0.1-beta27] - 2026-01-04

### Added

- **cli:** Enhance temps-cli with new commands and documentation generation
- **analytics:** Enhance analytics features with UTM tracking and visitor activity filtering
- **blob, kv:** Introduce temps-blob and temps-kv services with comprehensive functionality
- **cli, services:** Update temps-cli and introduce new services for blob and key-value storage

## [0.0.1-beta26] - 2026-01-02

### Added

- **email:** Add email validation service and update related components

### Miscellaneous

- **release:** Update macOS runner version from 13 to 15 for build jobs

## [0.0.1-beta25] - 2025-12-16

### Added

- **backup:** Integrate urlencoding for password handling in PostgreSQL connections
- **backup:** Add external service backup functionality
- **download-repo:** Enhance repository cloning logic with ref-based strategy
- **email:** Introduce email service with AWS SES and Scaleway support
- **email:** Add web interface for email management with Mailhog-like capture
- **email:** Integrate temps-email crate and register EmailPlugin
- **email:** Update EmailProvidersManagement with new icons and layout enhancements
- **email:** Add email management endpoints and SDK integration
- **email:** Add test email functionality for email providers
- **email:** Enhance test email functionality and error handling
- **deployment-tokens:** Implement deployment token management and validation
- **email:** Implement DNS verification utilities and enhance domain management
- **dns:** Introduce DNS management capabilities with Cloudflare and Namecheap support
- **dns:** Add DNS provider management and integration
- **env-vars:** Enhance environment variable management with new commands
- **docker:** Implement security hardening for Docker images using distroless
- **routes:** Add route management functionality with new AddRoute page
- **url-validation:** Introduce comprehensive URL validation to prevent SSRF attacks
- **build-image:** Enhance Docker image build process with project slug and prune command
- **vulnerability-scanner:** Introduce vulnerability scanning functionality
- **vulnerability-scanner:** Enhance vulnerability scanning with Docker image support
- **custom-domains:** Enhance redirect URL validation and error handling
- **vulnerability-scanner:** Add deployment_id to vulnerability scans and enhance related functionality
- **security:** Introduce vulnerability scanning features and UI components
- **vulnerability-scanner:** Add vulnerability scan completion notifications and trigger scan API
- **migrations:** Add environments route trigger for deployment updates
- **vulnerabilities:** Add new fields and notification handler for vulnerability scans
- **dns-providers:** Add support for Azure and Google Cloud DNS providers
- **dns:** Integrate DNS provider management and automatic DNS setup for email domains
- **setup:** Add initial setup command for Temps configuration
- **projects:** Revamp project creation process with enhanced user prompts and service integration
- **docker:** Transition to Alpine-based Node.js image for enhanced security
- **cli:** Enhance project creation with search-based repository selection and auto-sync
- **mcp:** Add tools handler and Temps API client integration
- **domains:** Add automatic DNS challenge record provisioning
- **setup:** Enhance setup command with git connection creation, geolite2 validation, and improved UX
- **cli:** Add runtime-logs command for container log streaming
- **cli:** Add environment configuration commands

### Fixed

- **backup:** Escape special characters in PostgreSQL password for Docker environment
- **backup:** Update error handling and content type for S3 uploads
- **network:** Always use container names for services and IPv4 for proxy

### Miscellaneous

- **workflows:** Comment out pull_request trigger in Rust tests workflow

### Refactor

- **docker:** Remove prune command for Next.js projects in Dockerfile generation
- **command-palette:** Format keywords for better readability in navigation items

## [0.0.1-beta24] - 2025-11-27

### Fixed

- **release:** Correct cache action version in release workflow

## [0.0.1-beta23] - 2025-11-26

### Added

- **backup:** Implement backup management commands in CLI
- **dependencies:** Update Cargo.toml and Cargo.lock for AWS SDK and new features
- **webhooks:** Add comprehensive diagnostic logging for webhook delivery troubleshooting
- **webhooks:** Add endpoint to retrieve specific webhook delivery details
- **temps-cli:** Set up API client and configuration for improved interaction

### Fixed

- **webhooks:** Start listener in background to avoid blocking plugin initialization

### Refactor

- **tests:** Improve Minio client configuration and clean up VisitorsList component

## [0.0.1-beta22] - 2025-11-17

### Refactor

- **digest:** Update notification digest structure and enhance project statistics

## [0.0.1-beta21] - 2025-11-17

### Added

- **query:** Introduce query service and enhance data browsing capabilities
- **ServiceDataBrowser:** Add icons for S3 bucket, prefix, and object types
- **metadata:** Enhance EntityInfo with additional metadata fields and improve error logging
- **docker:** Add Dockerfile for release and enhance CI/CD workflow
- **deployment:** Introduce deployment cancellation and preview environment support
- **activity-graph:** Add deployment activity graph endpoint and frontend component

## [0.0.1-beta20] - 2025-11-13

### Added

- **import:** Implement Docker container import functionality
- **projects:** Refactor ProjectServiceInfo to include detailed project metadata

### Fixed

- **logs:** Enhance log line and viewer components with text selection support
- **postgres:** Update default Docker image to postgres:18-alpine and adjust related configurations

## [0.0.1-beta19] - 2025-11-11

### Added

- **backup:** Auto-create S3 buckets when creating S3 targets
- Add live visitors feature and Docker registry settings
- Add preview environment functionality for projects
- **docker:** Add support for prebuilt binary to skip Rust compilation
- **docker:** Enhance Docker deployment with multi-stage builds and GeoLite2 management

### Fixed

- **docker:** Remove hardcoded database URL and improve Alpine compatibility
- **docker:** Add build tools and fix bun installation for Alpine Linux
- **docker:** Simplify Dockerfile by removing dumb-init and fix architecture compatibility

## [0.0.1-beta18] - 2025-11-10

### Refactor

- Streamline wasm-pack installation in CI workflows

## [0.0.1-beta17] - 2025-11-09

### Fixed

- Update CI workflows to ensure WASM build environment is correctly configured

## [0.0.1-beta16] - 2025-11-09

### Added

- Refactor project detail components and enhance sidebar functionality
- Enhance CI workflows and service management features

## [0.0.1-beta15] - 2025-11-09

### Added

- Implement container management and metrics retrieval features

## [0.0.1-beta14] - 2025-11-09

### Added

- Add integration and streaming tests for chunked transfer encoding
- Implement container metrics and management features

## [0.0.1-beta13] - 2025-11-08

### Added

- Enhance Docker testing and service configuration management

### Refactor

- Update test for solution verification with realistic difficulty

## [0.0.1-beta12] - 2025-11-06

### Added

- Add docker image configuration to Postgres service
- Refactor analytics to utilize proxy logs and introduce visitor ID tracking
- Add once_cell dependency and refactor network name handling
- Implement keyboard shortcut for adding environment variables
- Enhance environment variable logging and add new SDK files
- Improve component rendering and environment variable handling
- Add testing scripts and documentation for Temps MCP Server
- Add unique visitor count to ProjectCard component
- Optimize analytics query for overall stats
- Introduce CAPTCHA protection and IP access control features

### Fixed

- Correct total_count type in SessionLogsResponse

## [0.0.1-beta11] - 2025-10-27

### Added

- Enhance user management by excluding soft-deleted users and adding unique email constraint

### Fixed

- Update README and refactor cookie handling in middleware

## [0.0.1-beta10] - 2025-10-27

### Added

- Enhance authentication and user management logging
- Remove bcrypt support and enforce Argon2 for password hashing
- Enhance visitor analytics with user agent tracking
- Introduce KbdBadge component and implement keyboard shortcuts across various pages

## [0.0.1-beta9] - 2025-10-27

### Added

- Update static deployment paths and enhance TLS settings

## [0.0.1-beta8] - 2025-10-27

### Added

- Update installation instructions and release workflow

## [0.0.1-beta7] - 2025-10-27

### Added

- Enhance URL construction in DeploymentService
- Refine URL construction logic in DeploymentService
- Remove DomainProvisioning component and related routes
- Add error handling for service deletion with linked projects

## [0.0.1-beta6] - 2025-10-27

### Added

- Integrate default crypto provider for TLS in Domains Plugin

## [0.0.1-beta5] - 2025-10-27

### Added

- Revise release workflow and enhance deployment metadata

## [0.0.1-beta4] - 2025-10-27

### Added

- Update release workflow and dependencies

## [0.0.1-beta3] - 2025-10-27

### Added

- Introduce test release workflow and update main release configuration

## [0.0.1-beta2] - 2025-10-26

### Added

- Enhance release workflow with existing release deletion and file handling

## [0.0.1-beta1] - 2025-10-26

### Added

- Add go/python/flask as examples
- Add basic Flask application example
- Implement Java preset for Nixpacks
- Add timestamps option to container log queries
- Enhance static file deployment support in proxy service
- Add installation script and Homebrew formula generation to release workflow

### CI

- Update runner configuration for all tests to use ubuntu-latest-4-cores
- Enhance release workflow with macOS builds and disk cleanup steps

### Documentation

- Add CHANGELOG and release documentation
- Overhaul README for clarity and structure
- Enhance README with streamlined quick start guide
- Update README for improved PostgreSQL setup instructions

### Miscellaneous

- Update ci
- Optimize release profile and clean up CI workflows
- Update TimescaleDB image references to use latest version
- Update dependencies and enhance testing setup
- Enhance CI workflow with disk cleanup and deployment updates
- Update dependencies and enhance project structure
- Update go.sum in example go
- Update go.sum with new dependencies in gin-basic example
- Update dependencies and enhance project structure
- Update dependencies and improve project configurations
- Update dependencies and improve code structure
- Enable cargo formatting and clippy checks in pre-commit configuration
- Remove Nixpacks integration from temps-deployer
- Refactor migration files and update schema definitions

### Refactor

- Simplify log retrieval parameters with structured types
- Update integration tests to use new test configuration service
- Improve code formatting and readability across multiple files

### Testing

- Update log retrieval tests to use structured logging format

<!-- generated by git-cliff -->

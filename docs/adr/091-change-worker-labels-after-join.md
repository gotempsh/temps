# ADR-091: Changing a Worker's Labels After It Joined

- **Status:** Accepted
- **Date:** 2026-11-06
- **Builds on:** ADR-058 D7 (the worker decides its own ingress, in `agent.json`), ADR-090 (macOS workers)

## Context

A worker's scheduling labels (`region=eu-west`, `gpu=true`, and the reserved
`temps.sh/role=builder|dedicated`) are given to `temps join --labels` and saved
in the `labels` object of `agent.json`. The agent sends them in every heartbeat
and the control plane stores whatever it receives: the node asserts its own
labels, and nobody else can set them.

Changing them afterwards meant editing `agent.json` by hand and restarting the
agent, because the agent read them once, at start. That is error prone in four
ways:

- The JSON is hand-edited, in a file that also holds the node's token and
  certificate paths, and must stay `0600`.
- Nothing validated a label. `temps join --labels temps.sh/role=dedicatd`
  joined an ordinary worker without a word, and an entry without `=` was
  dropped silently. The control plane validates nothing either.
- `temps agent --labels` and `TEMPS_NODE_LABELS` override `agent.json`. An
  override in a systemd drop-in, a hand-edited LaunchAgent, or a foreground
  `temps agent --labels ...` makes the edit a silent no-op.
- A restart briefly stops everything the agent serves on the worker: its
  internal edge proxy, any public ingress it runs, the state stream. Labels
  are a scheduling fact; changing one should not drop traffic.

## Decision

### D1. `temps agent labels list | set | unset`

On the worker, as the user the agent runs as:

```
temps agent labels list
temps agent labels set temps.sh/role=dedicated gpu=true
temps agent labels unset gpu
```

`set` and `unset` rewrite only the `labels` object of `agent.json`, keeping
every other field, through a temporary file renamed over the original, mode
`0600`. `agent.json` is read as a bounded regular file, never through a
symlink; the temporary file has a name of the writing process's own and is
created exclusively; the rename happens only if `agent.json` is still the
file that was read; and an exclusive lock on `agent.json.lock` is held from
the read to the rename, so two overlapping edits take turns instead of one
losing the other's change. "Still the file that was read" means the same
inode, size and modification time, so a write in place is caught too.
`temps join` replaces `agent.json` under the same lock, through a temporary
file, so an edit overlapping a join cannot write the old enrollment back.
Temporary files a crashed run left behind are removed under the lock. The
agent keeps its labels when `agent.json` is not a JSON object at all. `--data-dir` picks
another agent directory, as for `temps agent ingress`, which uses the same
reader and writer.

### D2. Labels are validated, the same way everywhere

`temps_core::node_labels` holds the rules, and `temps join --labels` applies
them too, before anything is registered:

- a key is `name` or `prefix/name`: a name of 1-63 letters, digits, `-`, `_`
  or `.`, starting and ending with a letter or digit; a prefix of lowercase
  letters, digits, `-` and `.` (a DNS name), at most 253 characters, each
  part between dots at most 63;
- a value is at most 253 characters (not bytes), without control characters, commas
  (which `--labels` uses to separate labels) or surrounding whitespace; it may
  be empty;
- the `temps.sh/` prefix is reserved: only `temps.sh/role` exists, with the
  value `builder` or `dedicated`;
- a node has at most 64 labels.

The control plane still stores what a heartbeat carries. Rejecting there
would turn a hand-edited `agent.json` that works today into a node that
cannot heartbeat; validating at the two places an operator types labels is
the safe half. `unset` accepts any key already present, so a label written
before these rules can still be removed.

### D3. The running agent applies `agent.json` labels without a restart

The agent re-reads the `labels` object of `agent.json` every 5 seconds, as it
already does for `ingress`, and a change sends a heartbeat at once. A file
that does not parse, or whose `labels` is not an object, keeps the labels in
force, and so does a file that names another node ID or control plane URL
than the running agent's: a `temps join` that re-enrolled the directory while
the old agent still runs must not have its labels reported to the old
control plane. A file rewritten with the same labels (changed and changed
back between two reads) still counts as a change, so it is confirmed again.
Labels given with `--labels` or `TEMPS_NODE_LABELS` are pinned: the
agent never reloads them from the file.

### D4. The agent records the labels the control plane accepted

After a heartbeat succeeds with labels different from the last ones
recorded, the agent writes `labels-status.json` next to `agent.json`
(`0600`): the labels, whether they came from `agent.json` or an override, the
node ID, the control plane URL and the time. The command trusts a record only
for the node ID and control plane in `agent.json`, since node IDs repeat
across control planes. One small write per change, not per heartbeat.

This is how `set` and `unset` confirm the change locally: they wait (at most
15 seconds) for a record of exactly the new labels written after their own
write. `list` shows it next to `agent.json`, so a pending or ignored change
is visible.

### D5. Restart only an agent that cannot reload

An agent that has left a record (D4) reloads by itself, so it is never
restarted: when its confirmation is late (the control plane is unreachable,
say), the command says the change is pending and how to check it later. With
no record at all (an agent older than D3), `set` and `unset` restart the
Temps-managed service running from that data directory (the systemd unit or the `sh.temps.agent` LaunchAgent), with the
same checks `temps join` uses before restarting it, and wait up to a minute
for the record. `--no-restart` leaves the agent alone. When nothing confirms,
the command says exactly what to check: the node's Labels card in Settings >
Nodes.

### D6. An override is an error, not a silent no-op

Before writing, the command looks for an override where the agent would get
one: the installed systemd unit's effective environment and command line
(drop-ins included), the LaunchAgent plist, and the record (D4) of the agent
that last reported. Any of them fails the command, naming where the override
is and how to remove it, without touching `agent.json`. When the unit cannot
be checked (`systemctl show` fails, or an environment file it names cannot be
read) the command refuses too: an override it cannot rule out is treated as
one. `TEMPS_NODE_LABELS` in the
caller's own shell is reported as a warning: it only affects a `temps agent`
started from that shell.

### D7. Wrong machine, wrong user

No `agent.json` fails with where it looked and what to do: run `temps join`
first, pass `--data-dir`, or run as the right user. On macOS the agent runs
as the user who joined, so a `sudo` run is told to drop `sudo`; on Linux the
service usually runs as root, so a permission error says to use `sudo`.

Run as root, the command edits only a data directory that root alone
controls: the directory, every directory above it and `agent.json` must be
root's and not writable by others. Otherwise another user could swap files
under the root process and receive a copy of a root-only config (with its
node token) in a file they own. Such a directory belongs to a user, who runs
the command as themselves.

## Labels from the control plane: not now

An operator could reasonably expect to set labels from the console or
`temps node`. It is not built here:

- The node overwrites its labels on every heartbeat, so a control-plane
  value would be replaced within 30 seconds. It needs a second set of
  operator-owned labels, stored separately and merged at scheduling time,
  with a rule for a key both sides set.
- `temps.sh/role=dedicated` is a promise the worker's owner makes about the
  machine. Letting the control plane assert it changes who decides, and
  should be its own decision.

The console's Labels card instead says how to change labels, with the
command. Operator-owned labels are a follow-up if the need is real.

## Consequences

- Changing a role or label is one command, validated, and confirmed against
  the control plane without restarting a current agent.
- `temps join --labels` now fails on a label it used to accept silently
  (`temps.sh/role=worker`, `foo`). Every one of them was a mistake that did
  nothing.
- The agent reads `agent.json` every 5 seconds for labels, as it already
  did for ingress, and writes one small file per label change.

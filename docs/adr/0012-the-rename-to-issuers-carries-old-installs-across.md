# The rename to `issuers` carries old installs across

> **Since `docs/adr/0013`**, the data directory is `~/.config/issuers/`, not
> the platform data directory, so the second shim below now moves
> `…/issue-tracker/` (or `…/issuers/`) to `~/.config/issuers/`. The reasoning
> here — move once, never merge, keep `db_path` pure — is unchanged.

The project was renamed from `issue-tracker` to `issuers`, and unlike the
rename before it, this one ships code to keep old installs working. Two shims:

- `ISSUE_TRACKER_DB` is still honoured, beneath the new `ISSUERS_DB`.
- `Store::open` moves a lone `…/issue-tracker/` data directory to
  `…/issuers/` on first launch, and leaves it alone once the new one exists.

This reverses what the previous rename decided. Moving `gpui-issue-tracker` to
`issue-tracker` came with deliberately no migration code — "a one-time rename
is not worth a permanent fallback path in `db_path`, which is the single answer
to where the data is and should stay single" — and a `mv` in the commit
message for anyone carrying a database.

That was right when it was made: the only database it moved held three
settings and no Issues, so asking a person to move it by hand risked nothing.
It does not hold once a database has Issues in it: a missed `mv` no longer
opens an empty database harmlessly — it makes every Issue look deleted. And a stale
`ISSUE_TRACKER_DB` in a shell profile or an MCP client config is worse still:
it does not fail, it silently falls through to the *real* database, which is
the one thing that variable exists to prevent.

## Considered options

**No shims, a `mv` in the commit message**, as last time. Rejected for the
reasons above: the cost of a missed instruction has grown from nothing to
apparent data loss.

**A fallback search in `db_path`** — look in `issuers/`, then
`issue-tracker/`. Rejected because `db_path` would then have two answers, and
the CLI and MCP server would sometimes find `api.json` somewhere the app did
not write it. The directory move happens once, in `Store::open`, which only
the app calls; `db_path` stays a pure lookup with one answer.

**Merging two directories when both exist.** Rejected: choosing between two
databases is the person's call, not the app's.

## Consequences

The environment variable fallback *is* a permanent second name in `db_path`,
which is the half of the old objection that still stands. It is accepted
because it only ever narrows to a scratch database, never away from one.

Both shims can be deleted once no install predating the rename is left.

The `.mcp.json` server key is renamed too, `issues` to `issuers`, and this one
has no shim. A client names an MCP server's tools after its key, so every
`mcp__issues__*` tool becomes `mcp__issuers__*`, and any permission allowlist,
hook or prompt that names the old tools stops matching. Keeping `issues` was
considered — it names the domain rather than the binary — and rejected so
the server, its binary and the project share one name. The breakage is loud
rather than silent: an allowlist that no longer matches asks for permission
instead of granting it, so whatever named the old tools can be updated
when the prompt appears.

# The data directory is under `~/.config`, and binaries under `~/.local/bin`

Without `ISSUERS_DB`, `issues.db` and `api.json` now live in
`$XDG_CONFIG_HOME/issuers/`, which is `~/.config/issuers/` when that variable
is unset. It is the same on every platform. Previously the folder was the
platform data directory that `directories::ProjectDirs` chose, which on macOS
is `~/Library/Application Support/issuers/`. `scripts/install.sh` likewise
installs to `~/.local/bin` rather than `~/.cargo/bin`.

The person using this is a developer who works in a terminal, and these are
the paths they look in first. `~/Library/Application Support` is hidden from
Finder by default and needs quoting in a shell. `~/.cargo/bin` says how a
thing was built, not that it is installed.

## Considered options

**`~/.config` only for settings, with the database staying in the platform
data directory.** Rejected: there is no settings file to separate. Settings
are rows in the database (`docs/adr/0003`), so splitting the two would mean
splitting a table across a directory boundary.

**`$XDG_DATA_HOME` (`~/.local/share/issuers`) for the database**, which is
what the XDG spec says a database is. Rejected in favour of the one
directory a person asked for. The database holds their preferences as well
as their Issues, and one folder holding everything is easier to find, back
up and delete than the spec's distinction.

**Ignoring `$XDG_CONFIG_HOME` and hardcoding `~/.config`.** Rejected: anyone
who has set it has said where they want this to go. A relative or empty
value is ignored, as the spec says.

## Consequences

`Store::open` moves the first legacy directory that exists to the new one on
first launch, trying the more recent name first: `Application Support/issuers/`,
then `Application Support/issue-tracker/`. The order is by name, not by
modification time. As in `docs/adr/0012`, it never moves
anything once the new directory holds a database, and never merges two
databases. The test is the database rather than the directory: anything else
that writes under `~/.config` could leave an empty `issuers/` there, and that
would otherwise pass for an install and hide every Issue. Into an existing
directory it moves the files one at a time, `issues.db` last, so that an
interrupted move leaves no database behind and the next launch finishes it;
a name already present is refused, not overwritten. `~/.config` can be on a
different volume from `~/Library`, which `rename` cannot cross, so there it
copies each file and removes the original. Only the app moves
it, so `db_path` keeps one answer for the CLI and the MCP server.

A copy of the app built before this change still looks in the old place.
Running an old and a new build side by side therefore shows two different
databases. `scripts/install.sh` refuses to finish when a name resolves to
some other copy first, such as one an earlier version of the script left in
`~/.cargo/bin`, because that is how an old build keeps getting run.

`cargo install --root ~/.local` keeps Cargo's install records in
`~/.local/.crates.toml` and `~/.local/.crates2.json`, so `cargo uninstall`
needs the same `--root`.

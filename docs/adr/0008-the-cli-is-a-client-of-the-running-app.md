# The CLI is a client of the running app

`issuers-cli` talks to the local HTTP API over a socket rather than opening the
SQLite database itself. When the app is not running it fails, with its own exit
code (`3`) so a caller can tell "start Issuers" apart from a genuine failure.

This follows from ADR-0005. The projection is the single writer and lives in
the application process; a CLI that opened the database directly would be a
second writer, and any change it made would be invisible to an open window
until the next launch — the ADR-0002 failure mode, reached from a new
direction. Going through the API also means the CLI inherits every rule for
free: the completion invariant, one-level depth and case-folded Tag identity
are enforced once, in `Projection`, for all three surfaces.

## Considered options

Falling back to opening the database when nothing is listening was the real
alternative, and it is not absurd: the fallback would only run when there is no
other writer. It was rejected because it doubles the write paths — one through
`routes.rs`, one calling `Projection` directly — for two surfaces that must
stay identical, and because the check and the write are not atomic, so an app
launched in between would load a projection that is already stale. On macOS
this app deliberately outlives its window, so an instance you opened at all
today is still running and the error is rarer than it first appears.

Auto-launching the app was rejected because a terminal command that makes a
window appear is intrusive, and a headless start mode is a separate feature.

## Consequences

The crate gained a library, `issuers`, holding `domain`, `store`,
`projection`, `api` and `cli`. `ui` and `app_state` stay in the `Issuers`
binary, which is what makes the module seam structural rather than a
convention: nothing in the library can name the view. The CLI links no GPUI.

`GET /issues` grew a `parent` filter to serve `issuers-cli list --parent`, keeping
the rule that the API can answer everything a surface can ask. `Status` and
`Priority` parsing became case-insensitive, so `--status done` works — Tag
identity already folded case, and matching labels exactly was the odd one out.

A stale `api.json` left by a crash reports the same exit code as a missing one,
since the remedy is the same, but says so in different words.

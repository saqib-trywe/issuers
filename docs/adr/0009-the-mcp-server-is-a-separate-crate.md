# The MCP server is a separate crate

`issuers-mcp` is a workspace member building its own binary. It speaks the Model
Context Protocol on stdio, using the `rmcp` SDK, and reaches the tracker
through the local HTTP API — a client of the running app, exactly as `issuers-cli`
is, for exactly the reasons in ADR-0008.

It could not have lived inside `Issuers`. An MCP stdio server must own stdin and
stdout, which an app launched from the Dock does not have, and `rmcp` requires
tokio, which cannot share a process with GPUI's executor. But even where the
choice was open it went the same way: keeping the agent surface out of the app
means tool descriptions, argument schemas and protocol revisions can change
without touching the tracker, and MCP changes far more often than this app does.

## Considered options

**Streamable HTTP on the existing socket** was the tempting one: the listener,
the token and the guard already exist, and MCP defines that transport. It was
rejected because it would put every tool description and schema inside
`src/api/`, making the agent surface a change to the running app, and because
that guard refuses any request carrying `Origin` — a rule that exists to repel
browsers and would have started repelling legitimate clients instead.

**Hand-rolling JSON-RPC** would have matched how the HTTP server, the HTTP
client and the argument parser were each built. The reason those were
hand-rolled does not carry over. The HTTP this app speaks is frozen, on
loopback, in one shape: one request, one response, connection closed. MCP is
negotiated at the handshake and still moving — the SDK already carries four
protocol revisions and recognises a fifth. A hand-rolled server would be
correct against the revision it was written for, and would find out it wasn't
when a client upgraded.

**A third `[[bin]]` in the existing package** was rejected because binaries in
one package share one dependency table, so `rmcp` and tokio would be compiled
on every build of this repo — and any code under `src/` would sit in the
`issuers` library, whose whole value is that nothing heavy can get into
it. An async runtime there is the same erosion as a `use gpui`.

## Consequences

`src/cli/client.rs` became `src/client.rs`. The HTTP client is no longer the
CLI's; it has an error type of its own that distinguishes only what the
transport can — whether there was an app to talk to — and `cli::Failure` maps
that onto exit codes, which remain the CLI's business. Percent-encoding moved
with it, because "a slash survives in a path but not in a query value" is a
fact about this API's Tag names, and two clients encoding it differently would
disagree about which Tag they meant.

**The agent surface has no `delete_issue`.** This is not a change to the
domain: deleting still exists and means what it always meant, and the same
person can reach it from the window, the CLI or the API. It is withheld from
one client. Cancelling records a decision; deleting erases a mistake — "a typo,
a stray keystroke" — and that is a judgement about the person's own slip, which
an agent is not placed to make. An agent that thinks an Issue should go marks
it Cancelled, which leaves the decision visible.

Two things will break silently if forgotten. The bearer token is regenerated on
every launch of the app, so the address file is read on **every call** rather
than at startup; a server that cached it would keep working until you quit and
reopened Issues, and would then fail in a way that looks like an authentication
bug. And on stdio, stdout **is** the protocol channel: one stray `println!`,
from this code or a dependency, lands inside a JSON-RPC message and the client's
parser fails on it. Diagnostics go to stderr.

Tool arguments are declared with `deny_unknown_fields`. Without it, an agent
calling `update_issue` with `tags` would be told it succeeded and nothing would
change — the same trap `pico-args` sets for the CLI, which is why every command
there ends in a `finish()` check.

`issuers-mcp` depends on the root package for the client and the domain types,
and the root package depends on GPUI. So `cargo install --path crates/issuers-mcp`
compiles the whole GPUI tree to produce a binary that never links it. Nothing
is wrong with the result, but it is slow on a fresh machine. Making the GUI
dependencies optional behind a default-on feature, or moving the library into
its own crate, would fix it; neither was needed to ship this.

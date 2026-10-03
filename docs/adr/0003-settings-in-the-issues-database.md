# UI preferences live in the issues database

The theme picker needs to remember which light and dark themes were chosen.
Rather than introduce a config file, those preferences are stored in a
`setting(key TEXT PRIMARY KEY, value TEXT NOT NULL)` table in the same SQLite
database that holds Issues.

A future reader will reasonably wonder why UI state is sitting in a database
called `issues.db`, which is the main reason this is written down.

## Considered options

A config file in the platform config directory was the obvious alternative,
and it keeps the issues database purely about issues. It was rejected because
it means a second persistence mechanism: another file path to resolve, another
format to parse, another set of failure modes to handle, and another thing that
can disagree with the database about whether it exists. The store, the
migration runner, and a resolved file location already exist and are already
tested, so reusing them costs one migration and two methods.

## Consequences

The database is no longer exclusively about Issues, which slightly widens the
scope ADR-0002 describes. In exchange there remains exactly one file to back
up, one thing to point `ISSUERS_DB` at, and one persistence path to
reason about.

This is cheap to reverse if it stops paying: the table is one migration and the
data is a handful of rows.

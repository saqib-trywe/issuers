# Own SQLite store rather than a GitHub or GitLab client

The crate was first named `issue-tracker` (now `issuers`), which invited the
assumption that it fronts an existing tracker — it does not. This app owns its
data in a local SQLite database, with no network layer, no auth, and exactly
one user.

We considered driving the UI from the GitHub API, the GitLab API, or markdown
files on disk. Owning the store won because it makes the domain model ours to
define rather than inherited, and it keeps the whole data path synchronous and
offline — which in turn is what makes ADR-0002 possible.

## Consequences

Fields that exist only to coordinate between people carry no information here
and are deliberately absent: assignee, reporter, watchers, and comments. See
`CONTEXT.md`. If sync is ever added, that is a migration and a re-opening of
this decision, not a small feature.

// SPDX-License-Identifier: GPL-3.0-only

//! SQLite persistence for Issues.
//!
//! The UI keeps every Issue in memory and calls through here on mutation, so
//! nothing on the render path ever touches I/O. Queries are synchronous
//! because a local SQLite read of a few thousand rows is sub-millisecond and
//! `rusqlite` needs no async runtime alongside GPUI's own executor.

mod migrations;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};
use rusqlite::{Connection, OptionalExtension, Row};

use crate::domain::{Issue, IssueId, Tag};

/// Points the store at a scratch database during development so experiments
/// never touch real data.
pub const DB_PATH_ENV: &str = "ISSUE_TRACKER_DB";

const SELECT_COLUMNS: &str =
    "id, title, body, status, priority, created_at, updated_at, parent_id, size";

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the database at [`db_path`], creating parent directories and
    /// applying migrations.
    pub fn open() -> Result<Self> {
        let path = db_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }
        Self::open_at(&path)
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening database at {}", path.display()))?;
        Self::prepare(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    /// Runs raw SQL against the connection, so a test can make a write fail
    /// at a chosen point — `PRAGMA query_only`, or a trigger that aborts.
    /// A disk that fills or a file that locks cannot be arranged otherwise.
    #[cfg(test)]
    pub(crate) fn sabotage(&self, sql: &str) {
        self.conn.execute_batch(sql).expect("sabotage SQL");
    }

    fn prepare(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::migrations()
            .to_latest(&mut conn)
            .context("applying schema migrations")?;
        Ok(Self { conn })
    }

    /// Loads every Issue, Tags included. The caller owns the result and
    /// renders from it.
    pub fn load_all(&self) -> Result<Vec<Issue>> {
        let mut statement = self
            .conn
            .prepare(&format!("SELECT {SELECT_COLUMNS} FROM issue"))?;
        let mut issues = statement
            .query_map([], read_issue)?
            .collect::<Result<Vec<_>, _>>()?;

        let mut tags = self.load_tags()?;
        for issue in &mut issues {
            issue.tags = tags.remove(&issue.id).unwrap_or_default();
        }
        Ok(issues)
    }

    /// Every Issue's Tags in one pass, keyed by Issue.
    ///
    /// One query rather than one per Issue, so loading stays two statements
    /// however many Issues there are.
    fn load_tags(&self) -> Result<HashMap<IssueId, Vec<Tag>>> {
        let mut statement = self
            .conn
            .prepare("SELECT issue_id, name FROM issue_tag ORDER BY name COLLATE NOCASE")?;
        let rows = statement.query_map([], |row| {
            let id: IssueId = row.get(0)?;
            let name: String = row.get(1)?;
            Ok((id, parse_column::<Tag>(&name, 1)?))
        })?;

        let mut tags: HashMap<IssueId, Vec<Tag>> = HashMap::new();
        for row in rows {
            let (id, tag) = row?;
            tags.entry(id).or_default().push(tag);
        }
        Ok(tags)
    }

    /// Files a new Issue with every field it is born with, returning it as
    /// stored. The draft's `id` and timestamps are ignored: those are ours.
    ///
    /// One transaction for the row and its Tags, for the same reason as
    /// [`Self::update`] — and because a create that half-landed would hand
    /// the caller an error while leaving an Issue behind for a retry to
    /// duplicate.
    pub fn insert(&self, draft: &Issue) -> Result<Issue> {
        let now = format_timestamp(Utc::now());
        let tx = self.conn.unchecked_transaction()?;
        let mut issue = tx.query_row(
            &format!(
                "INSERT INTO issue
                    (title, body, status, priority, created_at, updated_at, parent_id, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)
                 RETURNING {SELECT_COLUMNS}"
            ),
            rusqlite::params![
                draft.title,
                draft.body,
                draft.status.label(),
                draft.priority.label(),
                now,
                draft.parent_id,
                draft.size,
            ],
            read_issue,
        )?;
        write_tags(&tx, issue.id, &draft.tags)?;
        tx.commit()?;

        issue.tags = draft.tags.clone();
        Ok(issue)
    }

    /// Writes every mutable field and stamps `updated_at`, returning the new
    /// timestamp so the caller can keep its in-memory copy in step.
    ///
    /// Transactional because Tags live in a second table: the row and its Tags
    /// have to move together, or a crash between the two leaves an Issue
    /// wearing the Tags of its previous revision. `unchecked_transaction`
    /// rather than `transaction` so this keeps its `&self` signature — the
    /// callers hold an immutable borrow of the projection while writing.
    pub fn update(&self, issue: &Issue) -> Result<DateTime<Utc>> {
        // At the precision it is stored with, so the copy in memory and the
        // row on disk hold the same instant rather than two a nanosecond
        // apart.
        let now = Utc::now().trunc_subsecs(6);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE issue
                SET title = ?2, body = ?3, status = ?4, priority = ?5, updated_at = ?6,
                    parent_id = ?7, size = ?8
              WHERE id = ?1",
            rusqlite::params![
                issue.id,
                issue.title,
                issue.body,
                issue.status.label(),
                issue.priority.label(),
                format_timestamp(now),
                issue.parent_id,
                issue.size,
            ],
        )?;

        write_tags(&tx, issue.id, &issue.tags)?;

        tx.commit()?;
        Ok(now)
    }

    /// Erases an Issue outright. Cancelling is a Status change, not this.
    pub fn delete(&self, id: IssueId) -> Result<()> {
        self.conn
            .execute("DELETE FROM issue WHERE id = ?1", rusqlite::params![id])?;
        Ok(())
    }

    /// Reads a UI preference. `None` when it has never been set.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM setting WHERE key = ?1",
                rusqlite::params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    /// Writes a UI preference, replacing any existing value.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO setting (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }
}

/// Makes an Issue's Tag rows say exactly `tags`.
///
/// Replaced wholesale rather than diffed: the Tag set is a handful of rows,
/// and a diff is more code and more ways to be wrong. Takes the transaction,
/// because on its own it is half of a change.
fn write_tags(tx: &rusqlite::Transaction<'_>, id: IssueId, tags: &[Tag]) -> Result<()> {
    tx.execute(
        "DELETE FROM issue_tag WHERE issue_id = ?1",
        rusqlite::params![id],
    )?;
    let mut insert = tx.prepare("INSERT INTO issue_tag (issue_id, name) VALUES (?1, ?2)")?;
    for tag in tags {
        insert.execute(rusqlite::params![id, tag.as_str()])?;
    }
    Ok(())
}

/// Preference keys.
pub mod settings_keys {
    /// Theme display names, as shown in the picker.
    pub const THEME_LIGHT: &str = "theme.light";
    pub const THEME_DARK: &str = "theme.dark";
    /// `"true"` / `"false"`. Absent means visible.
    pub const SIDEBAR_HIDDEN: &str = "sidebar.hidden";

    /// Working state, restored when the window is reopened. Unlike the keys
    /// above these are not preferences — they are where you happened to be.
    pub const UI_VIEW: &str = "ui.view";
    pub const UI_SELECTED: &str = "ui.selected";
    pub const UI_FILTER: &str = "ui.filter";
    /// The active Tag filter, or absent when none. Distinct from `UI_VIEW` —
    /// a Tag filter narrows *within* a View.
    pub const UI_TAG: &str = "ui.tag";
}

/// The database file location: `$ISSUE_TRACKER_DB` when set, otherwise the
/// platform data directory.
pub fn db_path() -> Result<PathBuf> {
    if let Some(override_path) = std::env::var_os(DB_PATH_ENV) {
        return Ok(PathBuf::from(override_path));
    }

    let dirs = directories::ProjectDirs::from("", "", "issue-tracker")
        .context("locating the platform data directory")?;
    Ok(dirs.data_dir().join("issues.db"))
}

/// Where the API publishes its port and token, beside the database so the two
/// live and die together.
///
/// Its presence means the app is running: it is written once the listener is
/// bound and removed on the way out.
pub fn api_file_path() -> Result<PathBuf> {
    Ok(db_path()?.with_file_name("api.json"))
}

fn format_timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_timestamp(raw: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, err.into())
        })
}

fn parse_column<T>(raw: &str, index: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    raw.parse().map_err(|err: T::Err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

fn read_issue(row: &Row<'_>) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        status: parse_column(&row.get::<_, String>(3)?, 3)?,
        priority: parse_column(&row.get::<_, String>(4)?, 4)?,
        // Filled in by `load_all`; a single `issue` row knows nothing of them.
        tags: Vec::new(),
        created_at: parse_timestamp(&row.get::<_, String>(5)?)?,
        updated_at: parse_timestamp(&row.get::<_, String>(6)?)?,
        parent_id: row.get(7)?,
        size: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Priority, Status};

    fn store() -> Store {
        Store::open_in_memory().expect("in-memory store")
    }

    /// Every other test here runs in memory, so nothing exercised the path
    /// the application actually takes: a file that does not exist yet, with
    /// the migrations run against it from scratch, and the data still there
    /// when it is reopened.
    #[test]
    fn a_store_on_disk_is_created_migrated_and_reopened() {
        let dir = std::env::temp_dir().join(format!("issue-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let path = dir.join("issues.db");

        let id = {
            let store = Store::open_at(&path).expect("a fresh file");
            let issue = store
                .insert(&Issue::draft("survives a restart"))
                .expect("insert");
            store
                .set_setting(settings_keys::UI_VIEW, "Doing")
                .expect("a setting");
            issue.id
        };
        assert!(path.exists(), "the file was created");

        // Reopened: the migration runner must be a no-op the second time, and
        // everything written must still be there.
        let store = Store::open_at(&path).expect("an existing file");
        let issues = store.load_all().expect("load");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, id);
        assert_eq!(issues[0].title, "survives a restart");
        assert_eq!(
            store
                .get_setting(settings_keys::UI_VIEW)
                .unwrap()
                .as_deref(),
            Some("Doing")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_store_is_empty() {
        assert!(store().load_all().unwrap().is_empty());
    }

    #[test]
    fn insert_defaults_to_todo_and_no_priority() {
        let store = store();
        let issue = store.insert(&Issue::draft("Wire up the sidebar")).unwrap();

        assert_eq!(issue.title, "Wire up the sidebar");
        assert_eq!(issue.body, "");
        assert_eq!(issue.status, Status::Todo);
        assert_eq!(issue.priority, Priority::None);
        assert_eq!(issue.created_at, issue.updated_at);
    }

    #[test]
    fn ids_are_sequential() {
        let store = store();
        let first = store.insert(&Issue::draft("first")).unwrap();
        let second = store.insert(&Issue::draft("second")).unwrap();
        assert_eq!(second.id, first.id + 1);
    }

    #[test]
    fn inserted_issues_survive_reload() {
        let store = store();
        store.insert(&Issue::draft("persisted")).unwrap();

        let loaded = store.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "persisted");
    }

    #[test]
    fn update_writes_every_mutable_field() {
        let store = store();
        let mut issue = store.insert(&Issue::draft("draft")).unwrap();

        issue.title = "sharpened".into();
        issue.body = "with a body".into();
        issue.status = Status::Blocked;
        issue.priority = Priority::Urgent;
        store.update(&issue).unwrap();

        let reloaded = store.load_all().unwrap().remove(0);
        assert_eq!(reloaded.title, "sharpened");
        assert_eq!(reloaded.body, "with a body");
        assert_eq!(reloaded.status, Status::Blocked);
        assert_eq!(reloaded.priority, Priority::Urgent);
    }

    #[test]
    fn update_advances_updated_at_but_not_created_at() {
        let store = store();
        let issue = store.insert(&Issue::draft("draft")).unwrap();

        let updated_at = store.update(&issue).unwrap();

        let reloaded = store.load_all().unwrap().remove(0);
        assert_eq!(reloaded.created_at, issue.created_at);
        assert!(reloaded.updated_at >= issue.updated_at);
        assert_eq!(
            format_timestamp(reloaded.updated_at),
            format_timestamp(updated_at)
        );
    }

    #[test]
    fn delete_removes_only_its_own_issue() {
        let store = store();
        let doomed = store.insert(&Issue::draft("typo")).unwrap();
        let kept = store.insert(&Issue::draft("real work")).unwrap();

        store.delete(doomed.id).unwrap();

        let remaining = store.load_all().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, kept.id);
    }

    fn tag(name: &str) -> Tag {
        name.parse().expect("valid tag")
    }

    /// Counts rows in the junction table directly, so cascade behaviour is
    /// checked against SQLite rather than against our own bookkeeping.
    fn tag_row_count(store: &Store) -> i64 {
        store
            .conn
            .query_row("SELECT count(*) FROM issue_tag", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn tags_survive_a_reload() {
        let store = store();
        let mut issue = store.insert(&Issue::draft("tagged")).unwrap();
        assert!(issue.tags.is_empty(), "a new Issue starts untagged");

        issue.tags = vec![tag("Bug"), tag("ui")];
        store.update(&issue).unwrap();

        let reloaded = store.load_all().unwrap().remove(0);
        assert_eq!(reloaded.tags, vec![tag("Bug"), tag("ui")]);
    }

    #[test]
    fn updating_replaces_the_tag_set_rather_than_adding_to_it() {
        let store = store();
        let mut issue = store.insert(&Issue::draft("retagged")).unwrap();

        issue.tags = vec![tag("bug"), tag("ui")];
        store.update(&issue).unwrap();
        issue.tags = vec![tag("docs")];
        store.update(&issue).unwrap();

        assert_eq!(store.load_all().unwrap().remove(0).tags, vec![tag("docs")]);
        assert_eq!(tag_row_count(&store), 1, "the old rows are gone");
    }

    #[test]
    fn clearing_every_tag_leaves_no_rows_behind() {
        let store = store();
        let mut issue = store.insert(&Issue::draft("untagged again")).unwrap();
        issue.tags = vec![tag("bug")];
        store.update(&issue).unwrap();

        issue.tags.clear();
        store.update(&issue).unwrap();

        assert_eq!(tag_row_count(&store), 0);
    }

    #[test]
    fn tags_belong_to_their_own_issue() {
        let store = store();
        let mut first = store.insert(&Issue::draft("first")).unwrap();
        let mut second = store.insert(&Issue::draft("second")).unwrap();
        first.tags = vec![tag("bug")];
        second.tags = vec![tag("ui")];
        store.update(&first).unwrap();
        store.update(&second).unwrap();

        let loaded = store.load_all().unwrap();
        let by_id = |id| {
            loaded
                .iter()
                .find(|issue: &&Issue| issue.id == id)
                .unwrap()
                .tags
                .clone()
        };
        assert_eq!(by_id(first.id), vec![tag("bug")]);
        assert_eq!(by_id(second.id), vec![tag("ui")]);
    }

    #[test]
    fn deleting_an_issue_takes_its_tags_with_it() {
        // Relies on the foreign key cascade, which only fires because
        // `prepare` turns `foreign_keys` on.
        let store = store();
        let mut doomed = store.insert(&Issue::draft("typo")).unwrap();
        doomed.tags = vec![tag("bug"), tag("ui")];
        store.update(&doomed).unwrap();
        assert_eq!(tag_row_count(&store), 2);

        store.delete(doomed.id).unwrap();

        assert_eq!(tag_row_count(&store), 0);
    }

    #[test]
    fn timestamps_round_trip_to_microsecond_precision() {
        let store = store();
        let issue = store.insert(&Issue::draft("precise")).unwrap();
        let reloaded = store.load_all().unwrap().remove(0);
        assert_eq!(
            format_timestamp(issue.created_at),
            format_timestamp(reloaded.created_at)
        );
    }

    #[test]
    fn unset_setting_reads_as_none() {
        assert_eq!(store().get_setting("theme.light").unwrap(), None);
    }

    #[test]
    fn settings_round_trip() {
        let store = store();
        store.set_setting("theme.light", "Gruvbox Light").unwrap();
        assert_eq!(
            store.get_setting("theme.light").unwrap(),
            Some("Gruvbox Light".to_string())
        );
    }

    #[test]
    fn setting_a_key_twice_overwrites_rather_than_failing() {
        let store = store();
        store.set_setting("theme.dark", "Gruvbox Dark").unwrap();
        store.set_setting("theme.dark", "Tokyo Night").unwrap();

        assert_eq!(
            store.get_setting("theme.dark").unwrap(),
            Some("Tokyo Night".to_string())
        );
    }

    #[test]
    fn settings_are_independent_of_each_other() {
        let store = store();
        store.set_setting("theme.light", "Solarized Light").unwrap();
        store.set_setting("theme.dark", "Solarized Dark").unwrap();

        assert_eq!(
            store.get_setting("theme.light").unwrap(),
            Some("Solarized Light".to_string())
        );
        assert_eq!(
            store.get_setting("theme.dark").unwrap(),
            Some("Solarized Dark".to_string())
        );
    }

    #[test]
    fn sidebar_hidden_round_trips() {
        let store = store();
        assert_eq!(
            store.get_setting(settings_keys::SIDEBAR_HIDDEN).unwrap(),
            None,
            "absent means visible"
        );

        store
            .set_setting(settings_keys::SIDEBAR_HIDDEN, "true")
            .unwrap();
        assert_eq!(
            store.get_setting(settings_keys::SIDEBAR_HIDDEN).unwrap(),
            Some("true".to_string())
        );

        store
            .set_setting(settings_keys::SIDEBAR_HIDDEN, "false")
            .unwrap();
        assert_eq!(
            store.get_setting(settings_keys::SIDEBAR_HIDDEN).unwrap(),
            Some("false".to_string())
        );
    }

    #[test]
    fn settings_do_not_disturb_issues() {
        let store = store();
        let issue = store.insert(&Issue::draft("unaffected")).unwrap();
        store.set_setting("theme.light", "Ayu Light").unwrap();

        let issues = store.load_all().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, issue.id);
    }

    #[test]
    fn env_override_takes_precedence_over_platform_dir() {
        // Guards the dev-safety property: with the override set, we never
        // resolve to the real data directory.
        unsafe { std::env::set_var(DB_PATH_ENV, "/tmp/scratch-issues.db") };
        let path = db_path().unwrap();
        unsafe { std::env::remove_var(DB_PATH_ENV) };

        assert_eq!(path, PathBuf::from("/tmp/scratch-issues.db"));
    }
}

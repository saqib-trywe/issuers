// SPDX-License-Identifier: GPL-3.0-only

//! Endpoint matching and the handlers behind it.
//!
//! Routing is written out by hand rather than delegated to a framework,
//! because one route needs matching a framework would fight: a Tag name is the
//! whole remainder of the path, slashes included, so `/issues/3/tags/ui/theme`
//! addresses the Tag `ui/theme` rather than being four segments.

use serde::de::DeserializeOwned;

use super::http::{Request, Response};
use super::wire::{IssueJson, NewIssue, PatchIssue, TagJson};
use crate::domain::{Issue, IssueId, Narrowing, ParentFilter, Status, Tag, View};
use crate::projection::{Projection, WriteError};

enum Route {
    Issues,
    Issue(IssueId),
    IssueTag(IssueId, String),
    IssueSubIssue(IssueId, IssueId),
    Tags,
}

/// The path template `docs/openapi.yaml` gives each route.
///
/// Exhaustive on purpose: a new route is a compile error here until it is
/// named, and the test beside `openapi.yaml`'s paths then fails until the
/// spec describes it too.
#[cfg(test)]
fn template(route: &Route) -> &'static str {
    match route {
        Route::Issues => "/issues",
        Route::Issue(_) => "/issues/{id}",
        Route::IssueTag(..) => "/issues/{id}/tags/{name}",
        Route::IssueSubIssue(..) => "/issues/{parent}/sub-issues/{child}",
        Route::Tags => "/tags",
    }
}

fn route(path: &str) -> Option<Route> {
    match path {
        "/issues" => return Some(Route::Issues),
        "/tags" => return Some(Route::Tags),
        _ => {}
    }

    let rest = path.strip_prefix("/issues/")?;
    let (id, tail) = match rest.find('/') {
        Some(cut) => (&rest[..cut], &rest[cut..]),
        None => (rest, ""),
    };
    let id: IssueId = id.parse().ok()?;

    if tail.is_empty() {
        return Some(Route::Issue(id));
    }
    // Matched before Tags: unlike a Tag name, a sub-issue is addressed by id,
    // so this segment is numeric and cannot contain a slash.
    if let Some(child) = tail.strip_prefix("/sub-issues/") {
        return child
            .parse()
            .ok()
            .map(|child| Route::IssueSubIssue(id, child));
    }
    // Everything after `/tags/` is the name, however many slashes it holds.
    let name = tail.strip_prefix("/tags/")?;
    if name.is_empty() {
        return None;
    }
    Some(Route::IssueTag(id, name.to_string()))
}

pub fn dispatch(request: &Request, projection: &mut Projection) -> Response {
    let Some(route) = route(&request.path) else {
        return Response::error(404, format!("no such endpoint: {}", request.path));
    };

    match (route, request.method.as_str()) {
        (Route::Issues, "GET") => list_issues(request, projection),
        (Route::Issues, "POST") => create_issue(request, projection),
        (Route::Issues, _) => method_not_allowed("GET, POST"),

        (Route::Issue(id), "GET") => match projection.get(id) {
            Some(issue) => Response::json(200, &issue_json(projection, issue)),
            None => not_found(id),
        },
        (Route::Issue(id), "PATCH") => patch_issue(id, request, projection),
        (Route::Issue(id), "DELETE") => match projection.delete(id) {
            Ok(()) => Response::empty(204),
            Err(err) => write_error(err),
        },
        (Route::Issue(_), _) => method_not_allowed("GET, PATCH, DELETE"),

        (Route::IssueTag(id, name), "PUT") => change_tag(id, &name, projection, true),
        (Route::IssueTag(id, name), "DELETE") => change_tag(id, &name, projection, false),
        (Route::IssueTag(..), _) => method_not_allowed("PUT, DELETE"),

        // `PUT` on an Issue that already belongs elsewhere *moves* it: the
        // request says where the child should end up, which is all a move is.
        (Route::IssueSubIssue(parent, child), "PUT") => {
            written(projection.set_parent(child, Some(parent)), projection)
        }
        (Route::IssueSubIssue(parent, child), "DELETE") => {
            match projection.get(child).map(|issue| issue.parent_id) {
                None => not_found(child),
                // Detaching from an Issue that does not hold it is a mistake
                // worth reporting rather than a silent success.
                Some(current) if current != Some(parent) => {
                    Response::error(409, format!("#{child} is not a sub-issue of #{parent}"))
                }
                Some(_) => written(projection.set_parent(child, None), projection),
            }
        }
        (Route::IssueSubIssue(..), _) => method_not_allowed("PUT, DELETE"),

        (Route::Tags, "GET") => list_tags(projection),
        (Route::Tags, _) => method_not_allowed("GET"),
    }
}

// ---- handlers ---------------------------------------------------------------

/// `GET /issues`, with the narrowings the window offers — a Status slice, a
/// Tag filter and a title substring — plus a Parent filter the window
/// expresses through the shape of its rows rather than as a control.
///
/// The narrowing itself is `domain::Narrowing`, shared with the window. All
/// this does is turn a query string into one, so the two surfaces cannot
/// disagree about what a filter means.
fn list_issues(request: &Request, projection: &Projection) -> Response {
    // An absent `?status=` is not a missing filter to remember: it is the
    // View that admits everything.
    let view = match request.param("status").map(str::parse::<Status>) {
        Some(Ok(status)) => View::WithStatus(status),
        Some(Err(err)) => return Response::error(400, err),
        None => View::All,
    };
    let tag = match request.param("tag").map(str::parse::<Tag>) {
        Some(Ok(tag)) => Some(tag),
        Some(Err(err)) => return Response::error(400, err),
        None => None,
    };
    let parent = match request.param("parent").map(str::parse::<ParentFilter>) {
        Some(Ok(parent)) => Some(parent),
        // The hint matters: `none` being a literal rather than an empty value
        // is not guessable, and this is where a caller finds out.
        Some(Err(err)) => {
            return Response::error(400, format!("{err} — use an issue id or \"none\""));
        }
        None => None,
    };

    let narrowing = Narrowing {
        view,
        tag,
        title: request.param("q").map(str::to_owned),
        parent,
    };

    // Worked out once for the whole corpus rather than scanned for per Issue,
    // which made a listing quadratic in the number of Issues it returned.
    let sub_issues = projection.sub_issue_index();

    // `issues()` is already in display order, and narrowing preserves it, so
    // the API and the window agree on what "first" means.
    let issues: Vec<IssueJson> = narrowing
        .select(projection.issues())
        .into_iter()
        .map(|issue| IssueJson::new(issue, sub_issues.of_issue(issue.id)))
        .collect();

    Response::json(200, &issues)
}

fn create_issue(request: &Request, projection: &mut Projection) -> Response {
    let new: NewIssue = match json_body(request) {
        Ok(body) => body,
        Err(response) => return response,
    };

    let title = new.title.trim().to_string();
    if title.is_empty() {
        return Response::error(400, "title must not be blank");
    }
    if new.rest.parent_id.is_some() {
        return Response::error(
            400,
            "use \"parent_id\" at the top level of the body, not inside a patch",
        );
    }
    // `PatchIssue` refuses these itself, but a flattened struct never sees the
    // keys it did not match, so for `POST` they arrive here instead. Refused
    // for the same reason: a create that silently dropped `"tagz"` reports
    // success and files an untagged Issue.
    if let Some(unknown) = new.unknown.keys().next() {
        return Response::error(400, format!("unknown field `{unknown}`"));
    }
    let patch = match new.rest.into_patch() {
        Ok(patch) => patch,
        Err(err) => return Response::error(400, err),
    };

    // One write, parent included: a refusing or missing parent is answered
    // before anything exists, and a failure part-way leaves no Issue behind
    // for the caller's retry to duplicate.
    let created = match projection.create(&title, patch, new.parent_id) {
        Ok(issue) => issue,
        Err(err) => return write_error(err),
    };

    Response::json(201, &issue_json(projection, &created))
        .with_header("Location", format!("/issues/{}", created.id))
}

fn patch_issue(id: IssueId, request: &Request, projection: &mut Projection) -> Response {
    let body: PatchIssue = match json_body(request) {
        Ok(body) => body,
        Err(response) => return response,
    };
    // Rejected loudly rather than ignored: a caller who thinks they moved an
    // Issue and did not would have no way to tell.
    if body.parent_id.is_some() {
        return Response::error(
            400,
            format!(
                "parent_id is not patchable; use PUT or DELETE /issues/{{parent}}/sub-issues/{id}"
            ),
        );
    }
    let mut patch = match body.into_patch() {
        Ok(patch) => patch,
        Err(err) => return Response::error(400, err),
    };
    // Held to the rule `POST` is. The window can blank a title, because
    // auto-save lands while you delete one to retype it; nothing that comes
    // through here is mid-keystroke.
    if let Some(title) = patch.title.as_mut() {
        *title = title.trim().to_string();
        if title.is_empty() {
            return Response::error(400, "title must not be blank");
        }
    }

    written(projection.patch(id, patch), projection)
}

/// `PUT`/`DELETE /issues/{id}/tags/{name}`.
///
/// Both are idempotent and both go through the single writer, so neither has
/// the read-modify-write race that changing one Tag through `PATCH` would.
fn change_tag(id: IssueId, name: &str, projection: &mut Projection, add: bool) -> Response {
    let tag: Tag = match name.parse() {
        Ok(tag) => tag,
        Err(err) => return Response::error(400, err),
    };

    let outcome = if add {
        projection.add_tag(id, tag)
    } else {
        projection.remove_tag(id, &tag)
    };

    written(outcome, projection)
}

/// `GET /tags` — the same derived list the sidebar shows, with the same counts.
fn list_tags(projection: &Projection) -> Response {
    let tags: Vec<TagJson> = projection
        .tags_in_use()
        .iter()
        .map(|tag| TagJson {
            name: tag.as_str().to_owned(),
            count: projection.count_with_tag(tag),
        })
        .collect();
    Response::json(200, &tags)
}

// ---- helpers ----------------------------------------------------------------

/// An absent body reads as `{}`, so `PATCH` with nothing to say is a no-op
/// rather than a parse error.
fn json_body<T: DeserializeOwned>(request: &Request) -> Result<T, Response> {
    let raw: &[u8] = if request.body.is_empty() {
        b"{}"
    } else {
        &request.body
    };
    serde_json::from_slice(raw).map_err(|err| Response::error(400, format!("invalid body: {err}")))
}

/// Renders an Issue together with the ids of its sub-issues.
fn issue_json(projection: &Projection, issue: &Issue) -> IssueJson {
    IssueJson::new(issue, &projection.sub_issues(issue.id))
}

/// Turns the outcome of a mutation into a response.
///
/// The three cases are deliberately distinct: 404 for a thing that is not
/// there, 409 for a request the data forbids, 500 for a database that failed.
fn written(outcome: Result<Issue, WriteError>, projection: &Projection) -> Response {
    match outcome {
        Ok(issue) => Response::json(200, &issue_json(projection, &issue)),
        Err(err) => write_error(err),
    }
}

fn write_error(err: WriteError) -> Response {
    match err {
        WriteError::NotFound(id) => not_found(id),
        WriteError::Refused(refusal) => Response::error(409, refusal),
        WriteError::Store(err) => Response::error(500, format!("{err:#}")),
    }
}

fn not_found(id: IssueId) -> Response {
    Response::error(404, format!("no issue #{id}"))
}

fn method_not_allowed(allow: &str) -> Response {
    Response::error(405, format!("allowed here: {allow}")).with_header("Allow", allow)
}

#[cfg(test)]
mod tests {
    use super::super::http::{Parsed, parse};
    use super::*;
    use crate::projection::IssuePatch;
    use crate::store::Store;

    fn projection() -> Projection {
        Projection::load(Store::open_in_memory().expect("store")).expect("projection")
    }

    /// Sends a request, skipping the auth guard (exercised in `auth`).
    fn send(projection: &mut Projection, method: &str, target: &str, body: &str) -> Response {
        let raw = if body.is_empty() {
            format!("{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        } else {
            format!(
                "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        match parse(raw.as_bytes()) {
            Parsed::Complete(request) => dispatch(&request, projection),
            other => panic!("could not build the request: {other:?}"),
        }
    }

    fn json(response: &Response) -> serde_json::Value {
        serde_json::from_slice(&response.body).expect("a JSON body")
    }

    #[test]
    fn an_unknown_path_is_not_found() {
        let mut p = projection();
        assert_eq!(send(&mut p, "GET", "/nope", "").status, 404);
        assert_eq!(send(&mut p, "GET", "/issues/notanumber", "").status, 404);
    }

    #[test]
    fn a_wrong_method_says_what_is_allowed() {
        let mut p = projection();
        let response = send(&mut p, "DELETE", "/issues", "");
        assert_eq!(response.status, 405);
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "Allow" && value.contains("POST"))
        );
    }

    #[test]
    fn creating_returns_201_and_where_it_went() {
        let mut p = projection();
        let response = send(&mut p, "POST", "/issues", r#"{"title":"filed by script"}"#);

        assert_eq!(response.status, 201);
        let id = json(&response)["id"].as_i64().unwrap();
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "Location" && value == &format!("/issues/{id}"))
        );
        assert_eq!(json(&response)["status"], "Todo");
    }

    #[test]
    fn creating_applies_every_field_in_one_call() {
        let mut p = projection();
        let response = send(
            &mut p,
            "POST",
            "/issues",
            r#"{"title":"full","body":"b","status":"Doing","priority":"Urgent","tags":["Bug","ui"]}"#,
        );

        assert_eq!(response.status, 201);
        let issue = json(&response);
        assert_eq!(issue["body"], "b");
        assert_eq!(issue["status"], "Doing");
        assert_eq!(issue["priority"], "Urgent");
        assert_eq!(issue["tags"], serde_json::json!(["Bug", "ui"]));
    }

    #[test]
    fn a_blank_title_is_refused() {
        let mut p = projection();
        assert_eq!(
            send(&mut p, "POST", "/issues", r#"{"title":"  "}"#).status,
            400
        );
        assert_eq!(send(&mut p, "POST", "/issues", "{}").status, 400);
    }

    /// The same rule by the other door. The window may hold a blank title
    /// while you retype one, but nothing out here is mid-keystroke.
    #[test]
    fn a_title_cannot_be_patched_blank_and_is_trimmed_like_a_new_one() {
        let mut p = projection();
        let id = p.create("kept", IssuePatch::default(), None).unwrap().id;
        let path = format!("/issues/{id}");

        let blank = send(&mut p, "PATCH", &path, r#"{"title":"  "}"#);
        assert_eq!(blank.status, 400);
        assert_eq!(p.get(id).unwrap().title, "kept");

        let padded = send(&mut p, "PATCH", &path, r#"{"title":"  renamed "}"#);
        assert_eq!(padded.status, 200);
        assert_eq!(p.get(id).unwrap().title, "renamed");
    }

    /// The failure the CLI's `finish()` and the MCP server's
    /// `deny_unknown_fields` exist to prevent, at the surface both of them are
    /// clients of: a misspelt field answering 200 and changing nothing is the
    /// one mistake a caller has no way to see.
    #[test]
    fn a_misspelt_field_is_refused_rather_than_ignored() {
        let mut p = projection();
        let id = make(&mut p, "unchanged");

        let response = send(
            &mut p,
            "PATCH",
            &format!("/issues/{id}"),
            r#"{"statuss":"Done"}"#,
        );

        assert_eq!(response.status, 400);
        assert!(
            json(&response)["error"]
                .as_str()
                .unwrap()
                .contains("statuss"),
            "the message names the offending field"
        );
        assert_eq!(p.get(id).unwrap().status, Status::Todo);
    }

    /// `POST` needs its own guard: `PatchIssue` arrives flattened, and a
    /// flattened struct never sees the keys it did not match.
    #[test]
    fn a_misspelt_field_is_refused_when_creating_too() {
        let mut p = projection();

        let response = send(&mut p, "POST", "/issues", r#"{"title":"t","tagz":["ui"]}"#);

        assert_eq!(response.status, 400);
        assert!(json(&response)["error"].as_str().unwrap().contains("tagz"));
        assert!(p.issues().is_empty(), "nothing was written");
    }

    #[test]
    fn a_bad_enum_value_is_a_400_naming_the_offender() {
        let mut p = projection();
        let response = send(
            &mut p,
            "POST",
            "/issues",
            r#"{"title":"t","status":"Wontfix"}"#,
        );
        assert_eq!(response.status, 400);
        assert!(
            json(&response)["error"]
                .as_str()
                .unwrap()
                .contains("Wontfix")
        );
    }

    #[test]
    fn malformed_json_is_a_400_not_a_panic() {
        let mut p = projection();
        assert_eq!(send(&mut p, "POST", "/issues", "{not json").status, 400);
    }

    #[test]
    fn reading_one_issue_or_failing_to() {
        let mut p = projection();
        let created = p.create("readable", IssuePatch::default(), None).unwrap();

        let found = send(&mut p, "GET", &format!("/issues/{}", created.id), "");
        assert_eq!(found.status, 200);
        assert_eq!(json(&found)["title"], "readable");

        assert_eq!(send(&mut p, "GET", "/issues/9999", "").status, 404);
    }

    #[test]
    fn a_patch_touches_only_what_it_names() {
        let mut p = projection();
        let created = p.create("keep me", IssuePatch::default(), None).unwrap();

        let response = send(
            &mut p,
            "PATCH",
            &format!("/issues/{}", created.id),
            r#"{"status":"Done"}"#,
        );

        assert_eq!(response.status, 200);
        assert_eq!(json(&response)["status"], "Done");
        assert_eq!(json(&response)["title"], "keep me");
    }

    #[test]
    fn an_empty_patch_body_is_a_no_op_rather_than_an_error() {
        let mut p = projection();
        let created = p.create("unchanged", IssuePatch::default(), None).unwrap();
        let response = send(&mut p, "PATCH", &format!("/issues/{}", created.id), "");
        assert_eq!(response.status, 200);
        assert_eq!(json(&response)["title"], "unchanged");
    }

    #[test]
    fn patching_a_missing_issue_is_404() {
        let mut p = projection();
        assert_eq!(
            send(&mut p, "PATCH", "/issues/404", r#"{"title":"ghost"}"#).status,
            404
        );
    }

    #[test]
    fn deleting_is_204_then_404() {
        let mut p = projection();
        let created = p.create("doomed", IssuePatch::default(), None).unwrap();
        let path = format!("/issues/{}", created.id);

        let gone = send(&mut p, "DELETE", &path, "");
        assert_eq!(gone.status, 204);
        assert!(gone.body.is_empty());
        assert_eq!(send(&mut p, "DELETE", &path, "").status, 404);
    }

    #[test]
    fn listing_is_in_display_order() {
        let mut p = projection();
        p.create("low", IssuePatch::default(), None).unwrap();
        let urgent = p
            .create(
                "urgent",
                IssuePatch::default().priority(crate::domain::Priority::Urgent),
                None,
            )
            .unwrap();

        let response = send(&mut p, "GET", "/issues", "");
        assert_eq!(json(&response)[0]["id"], urgent.id);
    }

    #[test]
    fn listing_narrows_by_status_tag_and_title() {
        let mut p = projection();
        p.create(
            "alpha bug",
            IssuePatch::default()
                .status(Status::Doing)
                .tags(vec!["Bug".parse().unwrap()]),
            None,
        )
        .unwrap();
        p.create("beta", IssuePatch::default().status(Status::Todo), None)
            .unwrap();

        let by_status = send(&mut p, "GET", "/issues?status=Doing", "");
        assert_eq!(json(&by_status).as_array().unwrap().len(), 1);

        let by_tag = send(&mut p, "GET", "/issues?tag=bug", "");
        assert_eq!(json(&by_tag).as_array().unwrap().len(), 1, "tags fold case");

        let by_title = send(&mut p, "GET", "/issues?q=BETA", "");
        assert_eq!(json(&by_title)[0]["title"], "beta");

        let combined = send(&mut p, "GET", "/issues?status=Todo&q=alpha", "");
        assert!(json(&combined).as_array().unwrap().is_empty());
    }

    #[test]
    fn a_bad_filter_value_is_a_400() {
        let mut p = projection();
        assert_eq!(send(&mut p, "GET", "/issues?status=Nope", "").status, 400);
        assert_eq!(send(&mut p, "GET", "/issues?parent=", "").status, 400);
        assert_eq!(send(&mut p, "GET", "/issues?parent=all", "").status, 400);
    }

    #[test]
    fn listing_narrows_by_parent() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let child = make(&mut p, "child");
        let loner = make(&mut p, "loner");
        p.set_parent(child, Some(parent)).unwrap();

        let under = send(&mut p, "GET", &format!("/issues?parent={parent}"), "");
        assert_eq!(json(&under).as_array().unwrap().len(), 1);
        assert_eq!(json(&under)[0]["id"], child);

        // The other half of the same question: what is not part of anything.
        let top = send(&mut p, "GET", "/issues?parent=none", "");
        let ids: Vec<i64> = json(&top)
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&parent) && ids.contains(&loner));

        // Composes with the other filters rather than replacing them.
        let none = send(
            &mut p,
            "GET",
            &format!("/issues?parent={parent}&q=loner"),
            "",
        );
        assert!(json(&none).as_array().unwrap().is_empty());
    }

    #[test]
    fn a_size_travels_and_a_null_takes_it_away() {
        let mut p = projection();
        let response = send(&mut p, "POST", "/issues", r#"{"title":"sized","size":7}"#);
        assert_eq!(json(&response)["size"], 7);
        let id = json(&response)["id"].as_i64().unwrap();

        // Absent leaves it alone; the rest of the patch still applies.
        let untouched = send(
            &mut p,
            "PATCH",
            &format!("/issues/{id}"),
            r#"{"status":"Doing"}"#,
        );
        assert_eq!(json(&untouched)["size"], 7);

        // Zero is a real Size, not an absence.
        let zeroed = send(&mut p, "PATCH", &format!("/issues/{id}"), r#"{"size":0}"#);
        assert_eq!(json(&zeroed)["size"], 0);
        assert_eq!(json(&zeroed)["total_size"], 0);

        // An explicit null unsizes it.
        let cleared = send(
            &mut p,
            "PATCH",
            &format!("/issues/{id}"),
            r#"{"size":null}"#,
        );
        assert_eq!(json(&cleared)["size"], serde_json::Value::Null);
        assert_eq!(json(&cleared)["total_size"], serde_json::Value::Null);
    }

    #[test]
    fn a_size_that_will_not_fit_is_refused() {
        let mut p = projection();
        // 255 is where an Issue should have become a tree of Issues.
        assert_eq!(
            send(&mut p, "POST", "/issues", r#"{"title":"huge","size":256}"#).status,
            400
        );
        assert_eq!(
            send(
                &mut p,
                "POST",
                "/issues",
                r#"{"title":"negative","size":-1}"#
            )
            .status,
            400
        );
    }

    #[test]
    fn a_parents_total_adds_its_parts_and_says_what_is_missing() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let sized = make(&mut p, "sized");
        let unmeasured = make(&mut p, "unsized");
        p.set_parent(sized, Some(parent)).unwrap();
        p.set_parent(unmeasured, Some(parent)).unwrap();
        p.patch(parent, IssuePatch::default().size(3)).unwrap();
        p.patch(sized, IssuePatch::default().size(5)).unwrap();

        let response = send(&mut p, "GET", &format!("/issues/{parent}"), "");
        assert_eq!(
            json(&response)["size"],
            3,
            "the work its parts do not cover"
        );
        assert_eq!(json(&response)["total_size"], 8);
        assert_eq!(json(&response)["unsized_sub_issues"], 1);

        // The whole fraction regardless of the narrowing, as with progress.
        let listed = send(&mut p, "GET", "/issues?parent=none", "");
        assert_eq!(listed_ids(&listed).len(), 1, "the parts are hidden");
        assert_eq!(json(&listed)[0]["total_size"], 8);
        assert_eq!(json(&listed)[0]["unsized_sub_issues"], 1);
    }

    fn listed_ids(response: &Response) -> Vec<i64> {
        json(response)
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["id"].as_i64().unwrap())
            .collect()
    }

    #[test]
    fn tags_are_added_and_removed_one_at_a_time() {
        let mut p = projection();
        let created = p.create("taggable", IssuePatch::default(), None).unwrap();
        let base = format!("/issues/{}/tags", created.id);

        let added = send(&mut p, "PUT", &format!("{base}/Bug"), "");
        assert_eq!(added.status, 200);
        assert_eq!(json(&added)["tags"], serde_json::json!(["Bug"]));

        // Idempotent, and case-folding: this is the same Tag.
        let again = send(&mut p, "PUT", &format!("{base}/bug"), "");
        assert_eq!(json(&again)["tags"], serde_json::json!(["Bug"]));

        let removed = send(&mut p, "DELETE", &format!("{base}/BUG"), "");
        assert_eq!(json(&removed)["tags"], serde_json::json!([]));
        // Removing what is already gone still succeeds.
        assert_eq!(
            send(&mut p, "DELETE", &format!("{base}/BUG"), "").status,
            200
        );
    }

    #[test]
    fn a_tag_name_may_contain_slashes_and_spaces() {
        let mut p = projection();
        let created = p.create("taggable", IssuePatch::default(), None).unwrap();
        let base = format!("/issues/{}/tags", created.id);

        let slashed = send(&mut p, "PUT", &format!("{base}/ui/theme"), "");
        assert_eq!(json(&slashed)["tags"], serde_json::json!(["ui/theme"]));

        let spaced = send(&mut p, "PUT", &format!("{base}/needs%20design"), "");
        assert_eq!(
            json(&spaced)["tags"],
            serde_json::json!(["needs design", "ui/theme"])
        );
    }

    #[test]
    fn an_unusable_tag_name_is_a_400() {
        let mut p = projection();
        let created = p.create("taggable", IssuePatch::default(), None).unwrap();
        let response = send(
            &mut p,
            "PUT",
            &format!("/issues/{}/tags/%20", created.id),
            "",
        );
        assert_eq!(response.status, 400);
    }

    #[test]
    fn tagging_a_missing_issue_is_404() {
        let mut p = projection();
        assert_eq!(send(&mut p, "PUT", "/issues/404/tags/Bug", "").status, 404);
    }

    // ---- sub-issues ---------------------------------------------------------

    fn make(p: &mut Projection, title: &str) -> crate::domain::IssueId {
        p.create(title, IssuePatch::default(), None).unwrap().id
    }

    #[test]
    fn attaching_and_detaching_over_http() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let child = make(&mut p, "child");

        let attached = send(
            &mut p,
            "PUT",
            &format!("/issues/{parent}/sub-issues/{child}"),
            "",
        );
        assert_eq!(attached.status, 200);
        assert_eq!(json(&attached)["parent_id"], parent);

        let parent_view = send(&mut p, "GET", &format!("/issues/{parent}"), "");
        assert_eq!(
            json(&parent_view)["sub_issue_ids"],
            serde_json::json!([child])
        );

        let detached = send(
            &mut p,
            "DELETE",
            &format!("/issues/{parent}/sub-issues/{child}"),
            "",
        );
        assert_eq!(detached.status, 200);
        assert_eq!(json(&detached)["parent_id"], serde_json::Value::Null);
    }

    #[test]
    fn putting_under_a_new_parent_moves_it() {
        let mut p = projection();
        let first = make(&mut p, "first");
        let second = make(&mut p, "second");
        let child = make(&mut p, "child");
        send(
            &mut p,
            "PUT",
            &format!("/issues/{first}/sub-issues/{child}"),
            "",
        );

        let moved = send(
            &mut p,
            "PUT",
            &format!("/issues/{second}/sub-issues/{child}"),
            "",
        );

        assert_eq!(moved.status, 200);
        assert_eq!(json(&moved)["parent_id"], second);
        let old = send(&mut p, "GET", &format!("/issues/{first}"), "");
        assert_eq!(json(&old)["sub_issue_ids"], serde_json::json!([]));
    }

    #[test]
    fn a_parent_reports_how_much_of_it_is_settled() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let done = make(&mut p, "done");
        let cancelled = make(&mut p, "cancelled");
        let outstanding = make(&mut p, "outstanding");
        for child in [done, cancelled, outstanding] {
            p.set_parent(child, Some(parent)).unwrap();
        }
        p.patch(done, IssuePatch::default().status(Status::Done))
            .unwrap();
        p.patch(cancelled, IssuePatch::default().status(Status::Cancelled))
            .unwrap();

        let response = send(&mut p, "GET", &format!("/issues/{parent}"), "");
        // Both count as settled: one finished, the other was deliberately
        // abandoned, and neither is waiting on anybody.
        assert_eq!(json(&response)["settled_sub_issues"], 2);
        assert_eq!(
            json(&response)["sub_issue_ids"].as_array().unwrap().len(),
            3
        );
    }

    #[test]
    fn an_issue_with_no_sub_issues_reports_none_settled() {
        let mut p = projection();
        let lonely = make(&mut p, "lonely");
        let response = send(&mut p, "GET", &format!("/issues/{lonely}"), "");
        assert_eq!(json(&response)["settled_sub_issues"], 0);
        assert_eq!(json(&response)["sub_issue_ids"], serde_json::json!([]));
    }

    #[test]
    fn a_narrowed_listing_still_carries_the_whole_fraction() {
        // The reason this is on the wire at all: a filter that hides the
        // children must not change the parent's fraction.
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let child = make(&mut p, "child");
        p.set_parent(child, Some(parent)).unwrap();
        p.patch(child, IssuePatch::default().status(Status::Done))
            .unwrap();

        let listed = send(&mut p, "GET", "/issues?parent=none", "");
        let issues = json(&listed);
        assert_eq!(issues.as_array().unwrap().len(), 1, "the child is hidden");
        assert_eq!(issues[0]["settled_sub_issues"], 1);
        assert_eq!(issues[0]["sub_issue_ids"], serde_json::json!([child]));
    }

    #[test]
    fn detaching_from_the_wrong_parent_is_reported() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let other = make(&mut p, "other");
        let child = make(&mut p, "child");
        send(
            &mut p,
            "PUT",
            &format!("/issues/{parent}/sub-issues/{child}"),
            "",
        );

        let wrong = send(
            &mut p,
            "DELETE",
            &format!("/issues/{other}/sub-issues/{child}"),
            "",
        );
        assert_eq!(wrong.status, 409);
    }

    #[test]
    fn rule_violations_are_409_not_400() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let child = make(&mut p, "child");
        let outsider = make(&mut p, "outsider");
        send(
            &mut p,
            "PUT",
            &format!("/issues/{parent}/sub-issues/{child}"),
            "",
        );

        // Depth, from both ends.
        assert_eq!(
            send(
                &mut p,
                "PUT",
                &format!("/issues/{child}/sub-issues/{outsider}"),
                ""
            )
            .status,
            409
        );
        assert_eq!(
            send(
                &mut p,
                "PUT",
                &format!("/issues/{outsider}/sub-issues/{parent}"),
                ""
            )
            .status,
            409
        );
        // Self-parenting.
        assert_eq!(
            send(
                &mut p,
                "PUT",
                &format!("/issues/{parent}/sub-issues/{parent}"),
                ""
            )
            .status,
            409
        );
        // Completing above outstanding work.
        let blocked = send(
            &mut p,
            "PATCH",
            &format!("/issues/{parent}"),
            r#"{"status":"Done"}"#,
        );
        assert_eq!(blocked.status, 409);
        assert!(
            json(&blocked)["error"]
                .as_str()
                .unwrap()
                .contains("outstanding")
        );
    }

    #[test]
    fn a_missing_issue_on_either_end_is_404() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        assert_eq!(
            send(
                &mut p,
                "PUT",
                &format!("/issues/{parent}/sub-issues/999"),
                ""
            )
            .status,
            404
        );
        assert_eq!(
            send(
                &mut p,
                "PUT",
                &format!("/issues/999/sub-issues/{parent}"),
                ""
            )
            .status,
            404
        );
    }

    #[test]
    fn creating_a_sub_issue_takes_one_call() {
        let mut p = projection();
        let parent = make(&mut p, "parent");

        let response = send(
            &mut p,
            "POST",
            "/issues",
            &format!(r#"{{"title":"child","parent_id":{parent}}}"#),
        );

        assert_eq!(response.status, 201);
        assert_eq!(json(&response)["parent_id"], parent);
    }

    /// A refusal that also created something is a refusal a caller cannot act
    /// on: retrying files the Issue twice, and the response never mentioned
    /// the first one.
    #[test]
    fn a_create_refused_by_its_parent_creates_nothing() {
        let mut p = projection();
        let held = make(&mut p, "already a sub-issue");
        let parent = make(&mut p, "parent");
        send(
            &mut p,
            "PUT",
            &format!("/issues/{parent}/sub-issues/{held}"),
            "",
        );
        let before = p.issues().len();

        // A parent that is itself a sub-issue: one level deep, so refused.
        let response = send(
            &mut p,
            "POST",
            "/issues",
            &format!(r#"{{"title":"grandchild","parent_id":{held}}}"#),
        );

        assert_eq!(response.status, 409);
        assert_eq!(p.issues().len(), before, "nothing was written");
    }

    #[test]
    fn a_create_naming_a_parent_that_does_not_exist_creates_nothing() {
        let mut p = projection();

        let response = send(
            &mut p,
            "POST",
            "/issues",
            r#"{"title":"orphan","parent_id":404}"#,
        );

        assert_eq!(response.status, 404);
        assert!(p.issues().is_empty(), "nothing was written");
    }

    #[test]
    fn patching_parent_id_points_at_the_right_endpoint() {
        // Silently ignoring it would leave a caller believing they moved
        // something they did not.
        let mut p = projection();
        let child = make(&mut p, "child");
        let parent = make(&mut p, "parent");

        let response = send(
            &mut p,
            "PATCH",
            &format!("/issues/{child}"),
            &format!(r#"{{"parent_id":{parent}}}"#),
        );

        assert_eq!(response.status, 400);
        assert!(
            json(&response)["error"]
                .as_str()
                .unwrap()
                .contains("sub-issues")
        );
    }

    #[test]
    fn a_wrong_method_on_a_sub_issue_says_what_is_allowed() {
        let mut p = projection();
        let parent = make(&mut p, "parent");
        let child = make(&mut p, "child");
        let response = send(
            &mut p,
            "GET",
            &format!("/issues/{parent}/sub-issues/{child}"),
            "",
        );
        assert_eq!(response.status, 405);
    }

    #[test]
    fn the_tag_list_matches_the_sidebar() {
        let mut p = projection();
        p.create(
            "one",
            IssuePatch::default().tags(vec!["Bug".parse().unwrap()]),
            None,
        )
        .unwrap();
        p.create(
            "two",
            IssuePatch::default().tags(vec!["bug".parse().unwrap(), "ui".parse().unwrap()]),
            None,
        )
        .unwrap();

        let response = send(&mut p, "GET", "/tags", "");
        assert_eq!(
            json(&response),
            serde_json::json!([{"name":"Bug","count":2},{"name":"ui","count":1}])
        );
    }

    // ---- the spec -----------------------------------------------------------

    /// Every path in `docs/openapi.yaml`, with the methods it documents.
    ///
    /// Read by hand rather than with a YAML parser: the spec is written by
    /// hand in one consistent shape, and this is the only thing that reads it.
    fn documented() -> Vec<(String, Vec<String>)> {
        let spec = include_str!("../../docs/openapi.yaml");
        let paths = spec
            .split_once("\npaths:\n")
            .expect("the spec has a paths section")
            .1;
        let mut documented: Vec<(String, Vec<String>)> = Vec::new();
        for line in paths.lines() {
            // The next top-level key ends the section.
            if !line.is_empty() && !line.starts_with(' ') {
                break;
            }
            if let Some(path) = line.strip_prefix("  /").and_then(|l| l.strip_suffix(':')) {
                documented.push((format!("/{path}"), Vec::new()));
            } else if let Some(method) = line.strip_prefix("    ").and_then(|l| l.strip_suffix(':'))
                && METHODS.contains(&method.to_ascii_uppercase().as_str())
            {
                let (_, methods) = documented.last_mut().expect("a method under a path");
                methods.push(method.to_ascii_uppercase());
            }
        }
        documented
    }

    const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

    /// A concrete path for a template, addressing things that exist.
    fn instantiate(template: &str) -> String {
        template
            .replace("{id}", "1")
            .replace("{parent}", "1")
            .replace("{child}", "2")
            // A slash inside, since a Tag name is the whole remainder.
            .replace("{name}", "ui/theme")
    }

    /// The failure CLAUDE.md warns of: a route added to the code and not the
    /// spec, or the other way about, and the spec quietly becoming fiction.
    #[test]
    fn the_openapi_spec_describes_exactly_the_routes_there_are() {
        let documented = documented();
        assert!(!documented.is_empty(), "no paths read from the spec");

        // Every route is documented, and nothing documented is missing.
        let witnesses = [
            Route::Issues,
            Route::Issue(1),
            Route::IssueTag(1, "ui".into()),
            Route::IssueSubIssue(1, 2),
            Route::Tags,
        ];
        let mut in_code: Vec<&str> = witnesses.iter().map(template).collect();
        let mut in_spec: Vec<&str> = documented.iter().map(|(path, _)| path.as_str()).collect();
        in_code.sort_unstable();
        in_spec.sort_unstable();
        assert_eq!(in_code, in_spec, "routes in the code vs paths in the spec");

        for (template_path, methods) in &documented {
            let path = instantiate(template_path);
            let reached = route(&path).unwrap_or_else(|| panic!("{path} reaches no route"));
            assert_eq!(
                template(&reached),
                template_path,
                "{path} reached another route"
            );

            for method in METHODS {
                // Fresh each time, so a DELETE cannot change what the next
                // method finds.
                let mut p = projection();
                let parent = p.create("parent", IssuePatch::default(), None).unwrap().id;
                let child = p.create("child", IssuePatch::default(), None).unwrap().id;
                assert_eq!((parent, child), (1, 2));

                let response = send(&mut p, method, &path, "");
                let allowed = methods.iter().any(|documented| documented == method);
                assert_eq!(
                    response.status != 405,
                    allowed,
                    "{method} {template_path}: the spec says {}, the code answered {}",
                    if allowed { "allowed" } else { "not allowed" },
                    response.status,
                );
                if response.status == 405 {
                    let (_, allow) = response
                        .headers
                        .iter()
                        .find(|(name, _)| name == "Allow")
                        .expect("a 405 says what is allowed");
                    let mut allow: Vec<&str> = allow.split(", ").collect();
                    let mut documented: Vec<&str> = methods.iter().map(String::as_str).collect();
                    allow.sort_unstable();
                    documented.sort_unstable();
                    assert_eq!(allow, documented, "the Allow header on {template_path}");
                }
            }
        }
    }
}

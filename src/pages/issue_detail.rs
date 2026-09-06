//! One issue in full: the exception chain with source context, what led up to it, and whatever an
//! agent has already concluded.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::components::toast::{ToastLevel, show_toast};
use crate::errors_data::{issue_detail, set_issue_status};
use crate::models::errors::{
    Analysis, Breadcrumb, ContextGroup, EventDetail, ExceptionValue, Frame, IssueTag, level_class,
};
use crate::models::repo_links::{SourceLinks, commit_url, compare_url};
use crate::routes::Route;

#[component]
pub fn IssueDetail(id: i64) -> Element {
    let mut issue = use_resource(move || async move { issue_detail(id).await });

    let update_status = move |status: &'static str, in_next_release: bool| async move {
        match set_issue_status(id, status.to_string(), in_next_release).await {
            Ok(()) => {
                let what = if in_next_release {
                    "Issue resolved until the next release".to_string()
                } else {
                    format!("Issue {status}")
                };
                show_toast(what, ToastLevel::Success);
                issue.restart();
            }
            Err(e) => show_toast(format!("Could not update: {e}"), ToastLevel::Error),
        }
    };

    match &*issue.read_unchecked() {
        None => rsx! {
            div { class: "max-w-5xl flex flex-col gap-4",
                div { class: "skeleton h-32" }
                div { class: "skeleton h-64" }
            }
        },
        Some(Err(e)) => rsx! {
            div { class: "max-w-5xl alert alert-error", "Could not load this issue: {e}" }
        },
        Some(Ok(detail)) => {
            let badge = level_class(&detail.level);
            rsx! {
                div { class: "max-w-5xl flex flex-col gap-6",

                    div {
                        Link {
                            to: Route::issues(detail.project_slug.clone()),
                            class: "inline-flex items-center gap-1.5 text-sm text-base-content/60 hover:text-base-content",
                            Icon { icon: LdArrowLeft, width: 14, height: 14 }
                            "{detail.project_slug}"
                        }
                        div { class: "flex flex-col sm:flex-row sm:items-start sm:justify-between gap-4 mt-3",
                            div { class: "min-w-0",
                                div { class: "flex items-center gap-2 flex-wrap",
                                    span { class: "badge {badge} gap-1.5",
                                        span { class: "w-1.5 h-1.5 rounded-full bg-current" }
                                        "{detail.level}"
                                    }
                                    span { class: "badge badge-ghost", "{detail.status}" }
                                    // The id the MCP tools take (`issue_id`), one click away so
                                    // "ask the agent about this issue" starts from the page.
                                    button {
                                        class: "badge badge-ghost font-mono gap-1 cursor-pointer hover:bg-base-300",
                                        title: "Copy issue id (what the MCP tools take as issue_id)",
                                        onclick: move |_| {
                                            let _ = document::eval(&format!(
                                                "navigator.clipboard.writeText('{id}')"
                                            ));
                                            show_toast(format!("Issue id {id} copied"), ToastLevel::Success);
                                        },
                                        "#{id}"
                                        Icon { icon: LdCopy, width: 12, height: 12 }
                                    }
                                }
                                h1 { class: "font-display text-2xl font-bold mt-2 break-words",
                                    "{detail.title}"
                                }
                                if let Some(culprit) = &detail.culprit {
                                    p { class: "font-mono text-sm text-base-content/60 mt-1", "{culprit}" }
                                }
                            }
                            div { class: "flex flex-wrap gap-2 shrink-0",
                                if detail.status == "resolved" {
                                    button {
                                        class: "btn btn-sm gap-1.5",
                                        onclick: move |_| async move { update_status("unresolved", false).await },
                                        Icon { icon: LdRotateCcw, width: 14, height: 14 }
                                        "Reopen"
                                    }
                                } else {
                                    button {
                                        class: "btn btn-sm btn-primary gap-1.5",
                                        onclick: move |_| async move { update_status("resolved", false).await },
                                        Icon { icon: LdCheck, width: 14, height: 14 }
                                        "Resolve"
                                    }
                                    // Only meaningful when the SDK reports releases: keeps the
                                    // issue resolved while the broken deploy is still out there,
                                    // reopening only when a newer release hits it.
                                    if detail.latest_event.as_ref().is_some_and(|e| e.release.is_some()) {
                                        button {
                                            class: "btn btn-sm",
                                            onclick: move |_| async move { update_status("resolved", true).await },
                                            "Resolve until next release"
                                        }
                                    }
                                    button {
                                        class: "btn btn-sm btn-ghost gap-1.5",
                                        onclick: move |_| async move { update_status("ignored", false).await },
                                        Icon { icon: LdBellOff, width: 14, height: 14 }
                                        "Ignore"
                                    }
                                }
                            }
                        }
                        div { class: "flex flex-wrap gap-x-10 gap-y-3 mt-5 rounded-xl border border-base-300 bg-base-200/60 px-5 py-3",
                            Fact { label: "Events", value: detail.times_seen.to_string() }
                            if detail.users_affected > 0 {
                                Fact {
                                    label: "Users",
                                    value: detail.users_affected.to_string(),
                                }
                            }
                            Fact { label: "First seen", value: short_time(&detail.first_seen) }
                            Fact { label: "Last seen", value: short_time(&detail.last_seen) }
                            // The upper bound on where the bug was introduced.
                            if let Some(release) = &detail.first_seen_release {
                                div {
                                    div { class: "text-xs uppercase tracking-wide text-base-content/50",
                                        "First seen in"
                                    }
                                    div { class: "mt-0.5",
                                        ReleaseRef {
                                            release: release.clone(),
                                            href: detail.repo_url.as_deref().and_then(|repo| commit_url(repo, release)),
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Before the analyses: for a regression the diff between the two releases is
                    // where the answer usually is, and an agent's findings should be read against it.
                    if let Some(good) = &detail.regressed_from_release {
                        RegressionRange {
                            good: good.clone(),
                            bad: detail.latest_event.as_ref().and_then(|e| e.release.clone()),
                            repo_url: detail.repo_url.clone(),
                        }
                    }

                    // Agent findings sit above the stack trace: if something already worked this
                    // out, reading it first is cheaper than re-deriving it.
                    if !detail.analyses.is_empty() {
                        section {
                            h2 { class: "text-sm font-semibold uppercase tracking-wide text-base-content/50 mb-2",
                                "Analysis"
                            }
                            div { class: "flex flex-col gap-3",
                                for analysis in detail.analyses.iter().cloned() {
                                    AnalysisCard { analysis }
                                }
                            }
                        }
                    }

                    // Distribution across *all* events, not just the one shown below — "every
                    // event has server_name=web-3" is a diagnosis in itself.
                    if !detail.tags.is_empty() {
                        TagsSection { tags: detail.tags.clone() }
                    }

                    match &detail.latest_event {
                        Some(event) => rsx! {
                            EventView { event: event.clone(), repo_url: detail.repo_url.clone() }
                        },
                        None => rsx! {
                            div { class: "alert", "This issue has no stored events." }
                        },
                    }
                }
            }
        }
    }
}

#[component]
fn TagsSection(tags: Vec<IssueTag>) -> Element {
    // The list arrives ordered by key, so consecutive rows with the same key form one group.
    let mut groups: Vec<(String, Vec<IssueTag>)> = Vec::new();
    for tag in tags {
        match groups.last_mut() {
            Some((key, values)) if *key == tag.key => values.push(tag),
            _ => groups.push((tag.key.clone(), vec![tag])),
        }
    }
    // Biggest slice first, so the bars read as a distribution.
    for (_, values) in groups.iter_mut() {
        values.sort_by_key(|t| std::cmp::Reverse(t.times_seen));
    }

    rsx! {
        section {
            h2 { class: "text-sm font-semibold uppercase tracking-wide text-base-content/50 mb-2",
                "Tags"
            }
            div { class: "grid gap-3 md:grid-cols-2",
                for (key , values) in groups {
                    {
                        let total: i64 = values.iter().map(|t| t.times_seen).sum::<i64>().max(1);
                        let hidden = values.len().saturating_sub(8);
                        rsx! {
                            div { class: "card bg-base-200 border border-base-300",
                                div { class: "card-body p-4 gap-2",
                                    div { class: "text-xs font-mono text-base-content/50", "{key}" }
                                    div { class: "flex flex-col gap-1",
                                        for tag in values.into_iter().take(8) {
                                            {
                                                let pct = tag.times_seen * 100 / total;
                                                rsx! {
                                                    div { class: "relative rounded-md overflow-hidden",
                                                        // Proportional fill: how much of this issue's
                                                        // traffic carries this value.
                                                        div {
                                                            class: "absolute inset-y-0 left-0 bg-primary/12",
                                                            style: "width: {pct}%",
                                                        }
                                                        div { class: "relative flex items-baseline gap-3 px-2 py-1 text-sm",
                                                            span { class: "flex-1 min-w-0 truncate", "{tag.value}" }
                                                            span { class: "font-medium tabular-nums shrink-0", "{pct}%" }
                                                            span { class: "text-xs text-base-content/50 tabular-nums shrink-0 w-10 text-right",
                                                                "×{tag.times_seen}"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        if hidden > 0 {
                                            div { class: "px-2 pt-1 text-xs text-base-content/40", "+{hidden} more" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A release name in mono, linked to its commit page when it names a revision in the repository.
#[component]
fn ReleaseRef(release: String, href: Option<String>) -> Element {
    match href {
        Some(url) => rsx! {
            a {
                class: "font-mono text-sm hover:text-primary hover:underline break-all",
                href: "{url}",
                target: "_blank",
                rel: "noopener noreferrer",
                title: "Open this commit in the repository",
                "{release}"
            }
        },
        None => rsx! {
            span { class: "font-mono text-sm break-all", "{release}" }
        },
    }
}

/// The last release the fix was verified against, and the one the issue is failing in again. The
/// diff between them is the change set that reintroduced the bug, so that link is the one action.
#[component]
fn RegressionRange(good: String, bad: Option<String>, repo_url: Option<String>) -> Element {
    let diff = match (&repo_url, &bad) {
        (Some(repo), Some(bad)) => compare_url(repo, &good, bad),
        _ => None,
    };

    rsx! {
        div { class: "flex flex-wrap items-center gap-x-4 gap-y-2 rounded-xl border border-warning/40 bg-warning/5 px-5 py-3",
            span { class: "inline-flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-warning",
                Icon { icon: LdGitCompare, width: 14, height: 14 }
                "Regression"
            }
            div { class: "flex flex-wrap items-baseline gap-x-2 gap-y-1 text-sm",
                span { class: "text-base-content/60", "last good" }
                ReleaseRef {
                    release: good.clone(),
                    href: repo_url.as_deref().and_then(|repo| commit_url(repo, &good)),
                }
                match &bad {
                    Some(bad) => rsx! {
                        span { class: "text-base-content/60", "→ failing in" }
                        ReleaseRef {
                            release: bad.clone(),
                            href: repo_url.as_deref().and_then(|repo| commit_url(repo, bad)),
                        }
                    },
                    None => rsx! {
                        span { class: "text-base-content/60", "→ the latest event names no release" }
                    },
                }
            }
            if let Some(url) = diff {
                a {
                    class: "btn btn-sm btn-outline btn-warning ml-auto gap-1.5",
                    href: "{url}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    "View the diff"
                    Icon { icon: LdExternalLink, width: 13, height: 13 }
                }
            }
        }
    }
}

#[component]
fn Fact(label: &'static str, value: String) -> Element {
    rsx! {
        div {
            div { class: "text-xs uppercase tracking-wide text-base-content/50", "{label}" }
            div { class: "font-display font-semibold tabular-nums mt-0.5", "{value}" }
        }
    }
}

#[component]
fn AnalysisCard(analysis: Analysis) -> Element {
    let confidence = analysis.confidence.clone().unwrap_or_default();
    let confidence_class = match confidence.as_str() {
        "high" => "badge-success",
        "medium" => "badge-warning",
        // Not `badge-ghost`: on a card it is the card's own colour, i.e. invisible.
        _ => "badge-neutral",
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300 border-l-4 border-l-primary",
            div { class: "card-body gap-2",
                div { class: "flex items-center gap-2 flex-wrap text-xs",
                    span { class: "text-primary", Icon { icon: LdBot, width: 16, height: 16 } }
                    span { class: "badge badge-sm badge-primary badge-outline", "{analysis.source}" }
                    if !confidence.is_empty() {
                        span { class: "badge badge-sm {confidence_class}", "{confidence} confidence" }
                    }
                    if let Some(release) = &analysis.release {
                        span { class: "font-mono text-base-content/50", "against {release}" }
                    }
                    span { class: "text-base-content/40 ml-auto", "{short_time(&analysis.created_at)}" }
                }
                p { class: "font-medium", "{analysis.summary}" }
                if let Some(details) = &analysis.details {
                    p { class: "text-sm text-base-content/70 whitespace-pre-wrap", "{details}" }
                }
                // The agent went past a diagnosis. Rendered as the card's one action, because a
                // pull request waiting for review is the thing a reader is here to act on.
                if let Some(url) = &analysis.fix_url {
                    div { class: "flex items-center gap-2 flex-wrap mt-1",
                        a {
                            class: "btn btn-sm btn-primary w-fit gap-2",
                            href: "{url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            Icon { icon: LdGitPullRequest, width: 14, height: 14 }
                            "Review the fix"
                        }
                        // Graded against production rather than asserted by the agent that wrote
                        // it — which is the only version of this claim worth showing.
                        match analysis.fix_verdict.as_deref() {
                            Some("held") => rsx! {
                                span { class: "badge badge-sm badge-success badge-outline",
                                    "fix held"
                                }
                            },
                            Some("regressed") => rsx! {
                                span { class: "badge badge-sm badge-error badge-outline",
                                    if let Some(release) = &analysis.regressed_in {
                                        "came back in {release}"
                                    } else {
                                        "came back"
                                    }
                                }
                            },
                            Some("pending") => rsx! {
                                span { class: "badge badge-sm badge-neutral",
                                    "no release since"
                                }
                            },
                            _ => rsx! {},
                        }
                    }
                }
                if let Some(fix) = &analysis.suggested_fix {
                    div { class: "rounded-lg border border-base-300 overflow-hidden mt-1",
                        div { class: "px-3 py-1.5 bg-base-300/60 text-xs uppercase tracking-wide text-base-content/50",
                            "Suggested fix"
                        }
                        pre { class: "bg-base-300/30 p-3 text-sm font-mono overflow-x-auto whitespace-pre-wrap",
                            "{fix}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn EventView(event: EventDetail, repo_url: Option<String>) -> Element {
    // Frames link into the repository only at a revision that exists there, so a version string
    // as the release means no links rather than links to `main` that may have moved since.
    let links = SourceLinks::new(repo_url.as_deref(), event.release.as_deref());

    rsx! {
        div { class: "flex flex-col gap-6",
            section {
                div { class: "flex flex-wrap gap-2",
                    if let Some(v) = &event.environment {
                        MetaChip { label: "env", value: v.clone() }
                    }
                    if let Some(v) = &event.release {
                        MetaChip { label: "release", value: v.clone() }
                    }
                    if let Some(v) = &event.transaction {
                        MetaChip { label: "in", value: v.clone() }
                    }
                    if let Some(v) = &event.server_name {
                        MetaChip { label: "on", value: v.clone() }
                    }
                    span { class: "self-center font-mono text-xs text-base-content/40",
                        "{event.event_id}"
                    }
                }
            }

            // Innermost exception first: that is the one that actually threw. Outer exceptions
            // follow under a "caused by" rule, reading the chain outwards.
            for (i , exception) in event.exception.iter().rev().cloned().enumerate() {
                if i > 0 {
                    div { class: "flex items-center gap-3 -my-2",
                        div { class: "h-px flex-1 bg-base-300" }
                        span { class: "text-xs uppercase tracking-wide text-base-content/40",
                            "caused by"
                        }
                        div { class: "h-px flex-1 bg-base-300" }
                    }
                }
                ExceptionView { exception, links: links.clone() }
            }

            if !event.breadcrumbs.is_empty() {
                section {
                    h2 { class: "text-sm font-semibold uppercase tracking-wide text-base-content/50 mb-2",
                        "Breadcrumbs"
                    }
                    div { class: "card bg-base-200 border border-base-300",
                        div { class: "card-body py-3 px-4",
                            div { class: "relative flex flex-col",
                                // Timeline rail behind the dots.
                                div { class: "absolute left-[3px] top-2 bottom-2 w-px bg-base-300" }
                                // Chronological, oldest first: breadcrumbs are the trail *into* the
                                // error, so the last row is what happened just before it broke.
                                // Reversing them reads the causality backwards.
                                for crumb in event.breadcrumbs.iter().cloned() {
                                    BreadcrumbRow { crumb }
                                }
                            }
                        }
                    }
                }
            }

            if !event.context.is_empty() {
                section {
                    h2 { class: "text-sm font-semibold uppercase tracking-wide text-base-content/50 mb-2",
                        "Context"
                    }
                    div { class: "grid gap-3 md:grid-cols-2",
                        for group in event.context.iter().cloned() {
                            ContextCard { group }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MetaChip(label: &'static str, value: String) -> Element {
    rsx! {
        span { class: "inline-flex items-baseline gap-1.5 rounded-md border border-base-300 bg-base-200/60 px-2 py-0.5 text-xs",
            span { class: "text-base-content/50", "{label}" }
            span { class: "font-mono", "{value}" }
        }
    }
}

/// How consecutive frames render: app frames (and every frame of an all-library trace) stand
/// alone; long runs of library frames collapse behind a toggle so the reader's eye goes straight
/// from one piece of their own code to the next.
enum FrameGroup {
    Single(Frame),
    Collapsed(Vec<Frame>),
}

/// Runs shorter than this render inline (dimmed) — a toggle row would be more UI than the frames
/// it hides.
const COLLAPSE_RUN_LEN: usize = 3;

fn group_frames(frames: Vec<Frame>) -> Vec<FrameGroup> {
    // A trace with no in-app frames at all (stdlib-only, or an SDK that never sets the flag)
    // must not vanish behind one giant toggle.
    let any_app = frames.iter().any(|f| f.in_app);
    let mut groups: Vec<FrameGroup> = Vec::new();
    let mut run: Vec<Frame> = Vec::new();

    let flush = |run: &mut Vec<Frame>, groups: &mut Vec<FrameGroup>| {
        if run.len() >= COLLAPSE_RUN_LEN {
            groups.push(FrameGroup::Collapsed(std::mem::take(run)));
        } else {
            groups.extend(run.drain(..).map(FrameGroup::Single));
        }
    };

    for (i, frame) in frames.into_iter().enumerate() {
        // The crashing frame (first row) always stays visible, in-app or not.
        if frame.in_app || !any_app || i == 0 {
            flush(&mut run, &mut groups);
            groups.push(FrameGroup::Single(frame));
        } else {
            run.push(frame);
        }
    }
    flush(&mut run, &mut groups);
    groups
}

#[component]
fn ExceptionView(exception: ExceptionValue, links: Option<SourceLinks>) -> Element {
    // Reversed so the crashing frame is at the top, where a reader looks first.
    let frames: Vec<Frame> = exception.frames.iter().rev().cloned().collect();

    rsx! {
        section {
            div { class: "flex items-baseline gap-2 flex-wrap mb-1",
                h2 { class: "font-mono font-semibold text-lg", "{exception.kind}" }
                if exception.handled == Some(false) {
                    span { class: "badge badge-sm badge-error", "unhandled" }
                }
            }
            if let Some(value) = &exception.value {
                p { class: "text-base-content/80 mb-3 break-words", "{value}" }
            }

            if frames.is_empty() {
                p { class: "text-sm text-base-content/50", "No stack trace." }
            } else {
                div { class: "card bg-base-200 border border-base-300 overflow-hidden",
                    div { class: "divide-y divide-base-300",
                        for group in group_frames(frames) {
                            match group {
                                FrameGroup::Single(frame) => rsx! {
                                    FrameView { frame, links: links.clone() }
                                },
                                FrameGroup::Collapsed(frames) => rsx! {
                                    LibraryFrames { frames, links: links.clone() }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LibraryFrames(frames: Vec<Frame>, links: Option<SourceLinks>) -> Element {
    let mut open = use_signal(|| false);
    let count = frames.len();

    rsx! {
        div {
            button {
                class: "w-full flex items-center gap-1.5 px-4 py-1.5 text-left text-xs text-base-content/45 hover:text-base-content/70 hover:bg-base-300/30",
                onclick: move |_| open.toggle(),
                if open() {
                    Icon { icon: LdChevronDown, width: 13, height: 13 }
                } else {
                    Icon { icon: LdChevronRight, width: 13, height: 13 }
                }
                "{count} library frames"
            }
            if open() {
                div { class: "divide-y divide-base-300 border-t border-base-300",
                    for frame in frames.iter().cloned() {
                        FrameView { frame, links: links.clone() }
                    }
                }
            }
        }
    }
}

#[component]
fn FrameView(frame: Frame, links: Option<SourceLinks>) -> Element {
    // App frames carry an accent bar and full-strength text; library frames are dimmed so the
    // reader's eye lands on their own code.
    let (accent, emphasis) = if frame.in_app {
        ("border-l-2 border-primary", "text-base-content")
    } else {
        ("border-l-2 border-transparent", "text-base-content/45")
    };
    let where_ = frame
        .filename
        .clone()
        .or(frame.module.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let line = frame.lineno.map(|n| format!(":{n}")).unwrap_or_default();
    let location = format!("{where_}{line}");
    let location_class = if frame.function.is_some() {
        "font-mono text-xs text-base-content/50 truncate"
    } else {
        "font-mono text-sm truncate"
    };
    // App frames only: a library's relative path (`django/db/models/query.py`) would link into
    // the wrong repository.
    let source = links
        .as_ref()
        .filter(|_| frame.in_app)
        .and_then(|links| links.file(frame.filename.as_deref()?, frame.lineno?));

    rsx! {
        div { class: "{accent}",
            div { class: "px-4 py-2 flex items-baseline gap-2 flex-wrap {emphasis}",
                if let Some(function) = &frame.function {
                    span { class: "font-mono text-sm font-semibold", "{function}" }
                }
                match &source {
                    Some(url) => rsx! {
                        a {
                            class: "{location_class} hover:text-primary hover:underline",
                            href: "{url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            title: "Open this line at the crashing revision",
                            "{location}"
                        }
                    },
                    None => rsx! {
                        span { class: "{location_class}", "{location}" }
                    },
                }
            }

            if frame.has_source() {
                div { class: "bg-base-300/50 font-mono text-xs overflow-x-auto",
                    {
                        // Line numbers count backwards from the offending line through pre_context
                        // and forwards through post_context.
                        let start = frame.lineno.unwrap_or(0) - frame.pre_context.len() as i64;
                        let mut rows = Vec::new();
                        for (i, line) in frame.pre_context.iter().enumerate() {
                            rows.push((start + i as i64, line.clone(), false));
                        }
                        if let Some(line) = &frame.context_line {
                            rows.push((frame.lineno.unwrap_or(0), line.clone(), true));
                        }
                        let after = frame.lineno.unwrap_or(0) + 1;
                        for (i, line) in frame.post_context.iter().enumerate() {
                            rows.push((after + i as i64, line.clone(), false));
                        }
                        rsx! {
                            for (number , line , is_culprit) in rows {
                                div {
                                    class: if is_culprit { "flex bg-error/10 border-l-2 border-error" } else { "flex border-l-2 border-transparent" },
                                    span {
                                        class: if is_culprit { "w-12 shrink-0 px-2 py-0.5 text-right text-error font-semibold select-none border-r border-base-300" } else { "w-12 shrink-0 px-2 py-0.5 text-right text-base-content/35 select-none border-r border-base-300" },
                                        "{number}"
                                    }
                                    span { class: "px-2 py-0.5 whitespace-pre", "{line}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn BreadcrumbRow(crumb: Breadcrumb) -> Element {
    let dot = match crumb.level.as_deref() {
        Some("fatal") | Some("error") => "bg-error",
        Some("warning") => "bg-warning",
        Some("info") => "bg-info",
        _ => "bg-base-content/30",
    };
    let time = crumb
        .timestamp
        .as_deref()
        .map(clock_time)
        .unwrap_or_default();

    rsx! {
        div { class: "relative pl-6 py-1.5 flex gap-3 items-baseline text-sm",
            span { class: "absolute left-0 top-[0.85rem] w-[7px] h-[7px] rounded-full {dot}" }
            span { class: "font-mono text-xs text-base-content/50 w-24 shrink-0 truncate",
                "{crumb.category.clone().unwrap_or_default()}"
            }
            span { class: "flex-1 min-w-0 break-words",
                "{crumb.message.clone().unwrap_or_default()}"
            }
            if !time.is_empty() {
                span { class: "font-mono text-xs text-base-content/40 tabular-nums shrink-0", "{time}" }
            }
        }
    }
}

#[component]
fn ContextCard(group: ContextGroup) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body p-4 gap-2",
                div { class: "text-xs uppercase tracking-wide text-base-content/50", "{group.title}" }
                dl { class: "text-sm flex flex-col gap-1",
                    for (key , value) in group.entries.iter().cloned() {
                        div { class: "flex gap-2",
                            dt { class: "font-mono text-base-content/60 shrink-0", "{key}" }
                            dd { class: "font-mono break-all text-right ml-auto", "{value}" }
                        }
                    }
                }
            }
        }
    }
}

/// `2026-07-29 10:00` — enough to orient, without the timezone noise of a full RFC 3339 stamp.
fn short_time(rfc3339: &str) -> String {
    rfc3339
        .split_once('T')
        .map(|(date, rest)| {
            let time = rest.get(..5).unwrap_or("");
            format!("{date} {time}")
        })
        .unwrap_or_else(|| rfc3339.to_string())
}

/// `10:00:42` — breadcrumbs happen seconds apart, so only the clock matters.
fn clock_time(rfc3339: &str) -> String {
    rfc3339
        .split_once('T')
        .and_then(|(_, rest)| rest.get(..8))
        .unwrap_or_default()
        .to_string()
}

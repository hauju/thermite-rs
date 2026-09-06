//! Issue list for one project: rate over time, current state, and what is broken.

use dioxus::prelude::*;
use dioxus_free_icons::{
    Icon,
    icons::ld_icons::{LdCopy, LdSettings},
};

use crate::UserAuthState;
use crate::components::copy_dsn::CopyDsn;
use crate::components::sparkline::{RateChart, Sparkline};
use crate::components::toast::{ToastLevel, show_toast, sleep};
use std::collections::BTreeSet;

use crate::errors_data::{
    components, environments, get_project, list_issues, list_monitors, project_stats,
    release_health, set_issues_status,
};
use crate::models::errors::{
    IssueQuery, IssueRow, MonitorRow, ProjectSummary, ReleaseHealthRow, level_class,
};
use crate::routes::{IssueFilters, Route};

/// Issues per fetch. The API caps a page at 100; this stays well under so a page is quick.
const PAGE: i64 = 50;

/// The keyboard listener for the list, installed once per visit and taken down again by
/// `KEY_LISTENER_REMOVE` when the page goes, so a stale handler never fires into a closed channel.
/// Keys typed into a field or aimed at a focused control are left alone.
const KEY_LISTENER: &str = r#"
    window.__thermiteIssueKeys = (e) => {
        const t = e.target;
        if (t && t.closest && t.closest('input, textarea, select, button, a, [contenteditable]')) return;
        if (e.metaKey || e.ctrlKey || e.altKey) return;
        if (!['j', 'k', 'Enter', 'x', 'r', 'i', 'Escape'].includes(e.key)) return;
        e.preventDefault();
        dioxus.send(e.key);
    };
    document.addEventListener('keydown', window.__thermiteIssueKeys);
"#;
const KEY_LISTENER_REMOVE: &str = r#"
    document.removeEventListener('keydown', window.__thermiteIssueKeys);
    delete window.__thermiteIssueKeys;
"#;

/// `filters` is the URL's view of the state; the signals below are the live one, seeded from it
/// on first render and mirrored back on every change.
#[component]
pub fn Issues(slug: String, filters: IssueFilters) -> Element {
    let seed = filters;
    // A visitor on the demo project sees the board and nothing that would change it.
    let auth = use_context::<Signal<UserAuthState>>();
    let can_write = matches!(&*auth.read(), UserAuthState::Authenticated(_));
    let mut status = use_signal(move || seed.status.unwrap_or_else(|| "unresolved".to_string()));
    let mut query = use_signal(move || seed.q.unwrap_or_default());
    let mut window = use_signal(move || seed.window.unwrap_or_else(|| "24h".to_string()));
    // Worst first, not newest first. The API defaults to `last_seen` to match Sentry, but for a
    // board that default buries a thousand-event outage under a three-event blip that happened to
    // fire more recently.
    let mut sort = use_signal(move || seed.sort.unwrap_or_else(|| "events".to_string()));
    let mut environment = use_signal(move || seed.env.unwrap_or_else(|| "all".to_string()));
    let mut component = use_signal(move || seed.component.unwrap_or_else(|| "all".to_string()));

    // Mirror the filters back into the URL, so a view survives a reload and can be shared or
    // linked to from an alert. `replace` rather than `push`: every keystroke in the search box
    // would otherwise become a history entry.
    let nav = use_navigator();
    use_effect({
        let slug = slug.clone();
        move || {
            nav.replace(Route::Issues {
                slug: slug.clone(),
                filters: IssueFilters {
                    status: Some(status()).filter(|v| v != "unresolved"),
                    sort: Some(sort()).filter(|v| v != "events"),
                    window: Some(window()).filter(|v| v != "24h"),
                    env: Some(environment()).filter(|v| v != "all"),
                    component: Some(component()).filter(|v| v != "all"),
                    q: Some(query().trim().to_string()).filter(|v| !v.is_empty()),
                },
            });
        }
    });

    // Only for the header: the display name, and slug as the subtitle — mirroring how every
    // card on /projects and /dashboard renders the pair.
    let mut project = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move { get_project(slug).await }
        }
    });

    let mut envs = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move { environments(slug).await }
        }
    });

    let mut comps = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move { components(slug).await }
        }
    });

    let mut stats = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            let window = window();
            async move { project_stats(slug, window).await }
        }
    });

    let mut monitors = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move { list_monitors(slug).await }
        }
    });

    let mut releases = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            let window = window();
            async move { release_health(slug, window).await }
        }
    });

    // One page of the list under the current filters. Reads the filter signals synchronously so
    // the resource below subscribes to them; the button that loads further pages reuses it.
    let list_page = {
        let slug = slug.clone();
        move |offset: i64| {
            let slug = slug.clone();
            let status = status();
            let query = query();
            let environment = environment();
            let component = component();
            let sort = sort();
            async move {
                list_issues(IssueQuery {
                    project: slug,
                    status: (status != "all").then_some(status),
                    query: (!query.trim().is_empty()).then(|| query.trim().to_string()),
                    environment: (environment != "all").then_some(environment),
                    component: (component != "all").then_some(component),
                    sort,
                    limit: PAGE,
                    offset,
                })
                .await
            }
        }
    };

    let mut issues = use_resource({
        let list_page = list_page.clone();
        move || list_page(0)
    });

    // Pages after the first, appended by "Load more". Offsets rather than a growing limit,
    // because the API caps a page at 100. Reset whenever a filter changes: the offsets are only
    // meaningful against the ordering they were fetched under.
    let mut more_rows = use_signal(Vec::<IssueRow>::new);
    let mut exhausted = use_signal(|| false);
    let mut loading_more = use_signal(|| false);
    // Ids ticked for a bulk action. Cleared with the list: a selection made under one filter
    // means nothing under another.
    let mut selected = use_signal(BTreeSet::<i64>::new);
    let mut reset_list = move || {
        more_rows.write().clear();
        exhausted.set(false);
        selected.write().clear();
    };

    // One status for everything ticked, then the list refetches so rows that no longer match
    // the filter drop out.
    let bulk = move |status: &'static str| {
        move |_| async move {
            let ids: Vec<i64> = selected.read().iter().copied().collect();
            let count = ids.len();
            match set_issues_status(ids, status.to_string()).await {
                Ok(()) => {
                    show_toast(
                        format!(
                            "{count} issue{} {status}",
                            if count == 1 { "" } else { "s" }
                        ),
                        ToastLevel::Success,
                    );
                    reset_list();
                    issues.restart();
                }
                Err(e) => show_toast(format!("Could not update: {e}"), ToastLevel::Error),
            }
        }
    };

    // Keyboard triage: j/k walk the list, Enter opens, x ticks, r resolves, i ignores, Escape
    // clears. One document-level listener rather than a focusable list, so it works without
    // clicking into the page first.
    let mut cursor = use_signal(|| None::<usize>);
    let visible_ids = move || -> Vec<i64> {
        let first = issues.peek();
        let Some(Ok(rows)) = &*first else {
            return Vec::new();
        };
        let more = more_rows.peek();
        rows.iter()
            .map(|r| r.id)
            .chain(
                more.iter()
                    .filter(|r| rows.iter().all(|f| f.id != r.id))
                    .map(|r| r.id),
            )
            .collect()
    };
    use_future(move || async move {
        if !cfg!(feature = "web") {
            return;
        }
        let mut keys = document::eval(KEY_LISTENER);
        while let Ok(key) = keys.recv::<String>().await {
            let ids = visible_ids();
            let at = cursor();
            let current = at.and_then(|i| ids.get(i).copied());
            match key.as_str() {
                "j" | "k" if !ids.is_empty() => {
                    let next = match (key.as_str(), at) {
                        ("j", Some(i)) => (i + 1).min(ids.len() - 1),
                        ("k", Some(i)) => i.saturating_sub(1),
                        _ => 0,
                    };
                    cursor.set(Some(next));
                    let _ = document::eval(&format!(
                        "document.querySelector('[data-issue-id=\"{}\"]')?.scrollIntoView({{ block: 'nearest' }})",
                        ids[next]
                    ));
                }
                "Enter" => {
                    if let Some(id) = current {
                        nav.push(Route::IssueDetail { id });
                    }
                }
                "x" | "r" | "i" if !matches!(&*auth.peek(), UserAuthState::Authenticated(_)) => {}
                "x" => {
                    if let Some(id) = current {
                        let mut set = selected.write();
                        if !set.remove(&id) {
                            set.insert(id);
                        }
                    }
                }
                "r" | "i" => {
                    if let Some(id) = current {
                        let status = if key == "r" { "resolved" } else { "ignored" };
                        match set_issues_status(vec![id], status.to_string()).await {
                            Ok(()) => {
                                show_toast(format!("Issue {status}"), ToastLevel::Success);
                                issues.restart();
                            }
                            Err(e) => {
                                show_toast(format!("Could not update: {e}"), ToastLevel::Error)
                            }
                        }
                    }
                }
                "Escape" => {
                    cursor.set(None);
                    selected.write().clear();
                }
                _ => {}
            }
        }
    });
    use_drop(move || {
        if cfg!(feature = "web") {
            let _ = document::eval(KEY_LISTENER_REMOVE);
        }
    });

    // The board keeps itself current. While nothing has reported yet it asks every few seconds,
    // so the tab a developer leaves open while wiring up an SDK turns into the board on its own
    // when the first event lands; after that it refetches every half minute, so an error storm
    // shows up without a reload. A restarted resource keeps its last value until the new one
    // arrives, so a refresh never flashes a skeleton. Ends once the project fails to load.
    use_future({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move {
                loop {
                    let waiting = matches!(&*project.peek(), Some(Ok(p)) if p.total_issues == 0);
                    sleep(if waiting { 5_000 } else { 30_000 }).await;

                    let (loaded, failed, waiting) = match &*project.peek() {
                        Some(Ok(p)) => (true, false, p.total_issues == 0),
                        Some(Err(_)) => (true, true, false),
                        None => (false, false, false),
                    };
                    if failed {
                        break;
                    }
                    if !loaded {
                        continue;
                    }
                    if waiting {
                        match get_project(slug.clone()).await {
                            Ok(p) if p.total_issues > 0 => {
                                // Everything was fetched against an empty project.
                                project.restart();
                                envs.restart();
                                comps.restart();
                                reset_list();
                            }
                            _ => continue,
                        }
                    }
                    stats.restart();
                    monitors.restart();
                    releases.restart();
                    issues.restart();
                }
            }
        }
    });

    // A project nothing has ever reported into gets setup instructions in place of an empty
    // board — zero stats over "nothing here" reads as broken, not as new.
    let first_event = match &*project.read_unchecked() {
        Some(Ok(p)) if p.total_issues == 0 => Some(p.clone()),
        _ => None,
    };

    rsx! {
        div { class: "max-w-6xl",
            div { class: "flex items-start justify-between gap-4 mb-6",
                div { class: "min-w-0",
                    h1 { class: "text-2xl font-bold truncate",
                        match &*project.read_unchecked() {
                            Some(Ok(p)) => rsx! { "{p.name}" },
                            // The slug is correct-if-plain, so the header never blocks on the fetch.
                            _ => rsx! { "{slug}" },
                        }
                    }
                    div { class: "text-xs text-base-content/50 font-mono truncate", "{slug}" }
                }
                div { class: "flex items-center gap-2 shrink-0",
                    div { class: "join",
                        for option in ["24h", "7d", "30d"] {
                            button {
                                class: if window() == option { "join-item btn btn-sm btn-primary" } else { "join-item btn btn-sm" },
                                onclick: move |_| window.set(option.to_string()),
                                "{option}"
                            }
                        }
                    }
                    if can_write {
                        Link {
                            to: Route::ProjectSettings { slug: slug.clone() },
                            class: "btn btn-sm btn-ghost btn-square",
                            "aria-label": "Project settings",
                            Icon { icon: LdSettings, width: 16, height: 16 }
                        }
                    }
                }
            }

            if let Some(fresh) = first_event {
                FirstEvent { project: fresh }
            } else {
                match &*stats.read_unchecked() {
                    Some(Ok(stats)) => rsx! {
                        div { class: "card bg-base-200 border border-base-300 mb-6",
                            div { class: "card-body gap-4",
                                div { class: "flex flex-wrap gap-6",
                                    Metric { label: "Events", value: stats.totals.events }
                                    Metric { label: "Unresolved", value: stats.totals.unresolved_issues }
                                    Metric { label: "New", value: stats.totals.new_issues }
                                    Metric { label: "Regressions", value: stats.totals.regressions }
                                    // Always rendered, unlike the monitors/releases panels: the zero
                                    // is the point — "nothing was silently lost", answered from
                                    // ingest day one rather than only once something goes missing.
                                    div {
                                        title: dropped_breakdown(&stats.totals.dropped_by_reason),
                                        div { class: "text-xs uppercase tracking-wide text-base-content/50",
                                            "Dropped"
                                        }
                                        div {
                                            class: if stats.totals.dropped > 0 { "text-2xl font-semibold tabular-nums text-error" } else { "text-2xl font-semibold tabular-nums text-base-content/40" },
                                            "{stats.totals.dropped}"
                                        }
                                    }
                                }
                                RateChart {
                                    counts: stats.series.iter().map(|b| b.count).collect::<Vec<_>>(),
                                    labels: stats.series.iter().map(|b| b.bucket.clone()).collect::<Vec<_>>(),
                                    resolution: stats.resolution.clone(),
                                    dropped: stats.series.iter().map(|b| b.dropped).collect::<Vec<_>>(),
                                }
                            }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "alert alert-error mb-6", "Could not load stats: {e}" }
                    },
                    None => rsx! {
                        div { class: "skeleton h-48 mb-6" }
                    },
                }

                // Only rendered once a job has checked in: an instance that runs no cron jobs should
                // not carry an empty panel forever.
                if let Some(Ok(list)) = &*monitors.read_unchecked()
                    && !list.is_empty()
                {
                    Monitors { monitors: list.clone() }
                }

                // Same rule: most SDKs never send sessions, and an empty release-health panel would
                // read as "no crashes" rather than "nothing is reporting".
                if let Some(Ok(list)) = &*releases.read_unchecked()
                    && list.iter().any(|r| r.sessions > 0)
                {
                    ReleaseHealth { releases: list.clone() }
                }

                div { class: "flex flex-wrap gap-2 mb-4",
                    div { class: "join",
                        for option in ["unresolved", "resolved", "ignored", "all"] {
                            button {
                                class: if status() == option { "join-item btn btn-sm btn-primary" } else { "join-item btn btn-sm" },
                                onclick: move |_| {
                                    reset_list();
                                    status.set(option.to_string());
                                },
                                "{option}"
                            }
                        }
                    }
                    div { class: "join",
                        for (value , label) in [("events", "most events"), ("last_seen", "most recent")] {
                            button {
                                class: if sort() == value { "join-item btn btn-sm btn-primary" } else { "join-item btn btn-sm" },
                                onclick: move |_| {
                                    reset_list();
                                    sort.set(value.to_string());
                                },
                                "{label}"
                            }
                        }
                    }
                    // Only worth showing once there is something to choose between.
                    if let Some(Ok(envs)) = &*envs.read_unchecked() {
                        if envs.len() > 1 {
                            select {
                                class: "select select-sm select-bordered w-auto",
                                onchange: move |e| {
                                    reset_list();
                                    environment.set(e.value());
                                },
                                option { value: "all", "all environments" }
                                for env in envs.iter().cloned() {
                                    option { value: "{env}", selected: environment() == env, "{env}" }
                                }
                            }
                        }
                    }
                    // Even one component is worth filtering on: events through the unlabeled
                    // default key carry no component tag, so "worker" vs everything-else is
                    // already a meaningful split.
                    if let Some(Ok(comps)) = &*comps.read_unchecked() {
                        if !comps.is_empty() {
                            select {
                                class: "select select-sm select-bordered w-auto",
                                onchange: move |e| {
                                    reset_list();
                                    component.set(e.value());
                                },
                                option { value: "all", "all components" }
                                for comp in comps.iter().cloned() {
                                    option { value: "{comp}", selected: component() == comp, "{comp}" }
                                }
                            }
                        }
                    }
                    input {
                        class: "input input-sm input-bordered flex-1 min-w-48",
                        r#type: "search",
                        placeholder: "Search titles…",
                        value: "{query}",
                        oninput: move |e| {
                            reset_list();
                            query.set(e.value());
                        },
                    }
                }

                match &*issues.read_unchecked() {
                    Some(Ok(rows)) if rows.is_empty() => rsx! {
                        div { class: "card bg-base-200 border border-base-300",
                            div { class: "card-body items-center text-center py-12",
                                p { class: "text-base-content/60",
                                    if query().trim().is_empty() {
                                        "Nothing here. Either nothing is broken, or nothing is reporting yet."
                                    } else {
                                        "No issues match that search."
                                    }
                                }
                            }
                        }
                    },
                    Some(Ok(rows)) => rsx! {
                        if !selected.read().is_empty() {
                            div { class: "flex items-center gap-2 flex-wrap rounded-xl border border-primary/40 bg-primary/5 px-4 py-2 mb-2 text-sm",
                                span { class: "font-medium",
                                    "{selected.read().len()} selected"
                                }
                                span { class: "ml-auto flex gap-2",
                                    button { class: "btn btn-sm btn-primary", onclick: bulk("resolved"), "Resolve" }
                                    button { class: "btn btn-sm btn-outline", onclick: bulk("ignored"), "Ignore" }
                                    button { class: "btn btn-sm btn-outline", onclick: bulk("unresolved"), "Reopen" }
                                    button {
                                        class: "btn btn-sm btn-ghost",
                                        onclick: move |_| selected.write().clear(),
                                        "Clear"
                                    }
                                }
                            }
                        }
                        div { class: "flex flex-col gap-2",
                            // A refresh of the first page can pull a row up out of the pages
                            // loaded after it; show it once.
                            for (i , row) in rows.iter().chain(more_rows.read().iter().filter(|r| rows.iter().all(|f| f.id != r.id))).cloned().enumerate() {
                                IssueCard {
                                    row,
                                    selected,
                                    highlighted: cursor() == Some(i),
                                    selectable: can_write,
                                }
                            }
                        }
                        // A full first page may have more behind it; a short one cannot.
                        if rows.len() as i64 == PAGE && !exhausted() {
                            div { class: "flex justify-center mt-3",
                                button {
                                    class: "btn btn-sm btn-outline",
                                    disabled: loading_more(),
                                    onclick: {
                                        let list_page = list_page.clone();
                                        let first = rows.len() as i64;
                                        move |_| {
                                            let page = list_page(first + more_rows.read().len() as i64);
                                            async move {
                                                loading_more.set(true);
                                                match page.await {
                                                    Ok(next) => {
                                                        if (next.len() as i64) < PAGE {
                                                            exhausted.set(true);
                                                        }
                                                        more_rows.write().extend(next);
                                                    }
                                                    Err(e) => show_toast(
                                                        format!("Could not load more: {e}"),
                                                        ToastLevel::Error,
                                                    ),
                                                }
                                                loading_more.set(false);
                                            }
                                        }
                                    },
                                    if loading_more() {
                                        span { class: "loading loading-spinner loading-xs" }
                                    }
                                    "Load more"
                                }
                            }
                        }
                        div { class: "hidden md:flex flex-wrap items-center gap-x-4 gap-y-1 mt-3 text-xs text-base-content/40",
                            span { kbd { class: "kbd kbd-xs", "j" } " " kbd { class: "kbd kbd-xs", "k" } " move" }
                            span { kbd { class: "kbd kbd-xs", "↵" } " open" }
                            if can_write {
                                span { kbd { class: "kbd kbd-xs", "x" } " select" }
                                span { kbd { class: "kbd kbd-xs", "r" } " resolve" }
                                span { kbd { class: "kbd kbd-xs", "i" } " ignore" }
                            }
                            span { kbd { class: "kbd kbd-xs", "esc" } " clear" }
                        }
                    },
                    Some(Err(e)) => rsx! {
                        div { class: "alert alert-error", "Could not load issues: {e}" }
                    },
                    None => rsx! {
                        div { class: "flex flex-col gap-2",
                            for _ in 0..3 {
                                div { class: "skeleton h-20" }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// `over_quota: 3, invalid: 1` for the Dropped metric's hover, or a hint when nothing dropped.
fn dropped_breakdown(by_reason: &std::collections::BTreeMap<String, i64>) -> String {
    if by_reason.is_empty() {
        return "events rejected by quota, unsupported, unparseable, or discarded by the SDK"
            .to_string();
    }
    by_reason
        .iter()
        .map(|(reason, count)| format!("{reason}: {count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a project nothing has reported into shows instead of an empty board: the DSN, a snippet
/// per SDK with it filled in, and a way to raise a test event without one.
#[component]
fn FirstEvent(project: ProjectSummary) -> Element {
    let mut lang = use_signal(|| "python");
    let (install, code) = snippet(lang(), &project.dsn);
    // A JSON string is a valid JS string literal, quotes and newlines included.
    let copy_code = serde_json::Value::String(code.clone()).to_string();

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-5",
                div {
                    h2 { class: "card-title text-base", "Send your first event" }
                    p { class: "text-sm text-base-content/60",
                        "Nothing has reported into this project yet. Point an SDK at its DSN — "
                        "this page turns into the board on its own when the first event lands."
                    }
                }
                div {
                    div { class: "text-xs uppercase tracking-wide text-base-content/50 mb-1", "DSN" }
                    div { class: "flex items-center gap-2",
                        code { class: "flex-1 bg-base-300 rounded px-3 py-2 text-xs break-all font-mono",
                            "{project.dsn}"
                        }
                        CopyDsn { dsn: project.dsn.clone(), label: project.slug.clone() }
                    }
                }
                div { class: "flex flex-col gap-2",
                    div { class: "flex items-center justify-between gap-2 flex-wrap",
                        div { class: "join",
                            for (value , label) in [("python", "Python"), ("javascript", "JavaScript"), ("rust", "Rust")] {
                                button {
                                    // `btn-outline` rather than a bare `btn`: on a card the bare
                                    // one is the card's own colour.
                                    class: if lang() == value { "join-item btn btn-sm btn-primary" } else { "join-item btn btn-sm btn-outline" },
                                    onclick: move |_| lang.set(value),
                                    "{label}"
                                }
                            }
                        }
                        button {
                            class: "btn btn-sm btn-ghost gap-1.5",
                            onclick: move |_| {
                                let _ = document::eval(&format!(
                                    "navigator.clipboard.writeText({copy_code})"
                                ));
                                show_toast("Snippet copied", ToastLevel::Success);
                            },
                            Icon { icon: LdCopy, width: 14, height: 14 }
                            "Copy"
                        }
                    }
                    div { class: "rounded-lg border border-base-300 overflow-hidden",
                        div { class: "px-3 py-1.5 bg-base-300/60 font-mono text-xs text-base-content/60",
                            "{install}"
                        }
                        pre { class: "bg-base-300/30 p-3 text-xs font-mono overflow-x-auto", "{code}" }
                    }
                }
                div { class: "flex items-center gap-3 flex-wrap text-sm",
                    span { class: "inline-flex items-center gap-2 text-base-content/60",
                        span { class: "loading loading-spinner loading-xs" }
                        "Waiting for the first event…"
                    }
                    span { class: "ml-auto flex gap-2",
                        Link {
                            to: Route::DocsPage { slug: vec!["guides".into(), "sdks".into()] },
                            class: "btn btn-sm btn-outline",
                            "SDK guide"
                        }
                        Link {
                            to: Route::Playground {},
                            class: "btn btn-sm btn-primary",
                            "Raise a test event"
                        }
                    }
                }
            }
        }
    }
}

/// The install line and the init snippet for one SDK, with the project's DSN in place. Mirrors
/// the SDK guide in the docs; keep the two in step.
fn snippet(lang: &str, dsn: &str) -> (&'static str, String) {
    match lang {
        "javascript" => (
            "bun add @sentry/browser   # or @sentry/node",
            format!(
                r#"import * as Sentry from "@sentry/browser";

Sentry.init({{
  dsn: "{dsn}",
  release: "myapp@1.4.2",       // or the git SHA
  environment: "production",
  tracesSampleRate: 0,          // Thermite stores no transactions
}});"#
            ),
        ),
        "rust" => (
            r#"thermite-sdk = { git = "https://github.com/hauju/thermite-rs" }"#,
            format!(
                r#"let mut options = thermite_sdk::Options::new("{dsn}");
options.release = Some(env!("CARGO_PKG_VERSION").to_string());   // or the git SHA
options.environment = Some("production".into());

// Held for the life of `main`: dropping it flushes whatever is still queued.
let _guard = thermite_sdk::init(options)?;"#
            ),
        ),
        _ => (
            "pip install sentry-sdk",
            format!(
                r#"import sentry_sdk

sentry_sdk.init(
    dsn="{dsn}",
    release="myapp@1.4.2",          # or the git SHA
    environment="production",
    traces_sample_rate=0.0,          # Thermite stores no transactions
)"#
            ),
        ),
    }
}

#[component]
fn Metric(label: &'static str, value: i64) -> Element {
    rsx! {
        div {
            div { class: "text-xs uppercase tracking-wide text-base-content/50", "{label}" }
            div { class: "text-2xl font-semibold tabular-nums", "{value}" }
        }
    }
}

/// Sessions per release and how many of them crashed.
///
/// The point of the panel is the denominator: an error count rises with traffic, so it cannot tell
/// a broken release from a busy one. Crash-free rate can.
#[component]
fn ReleaseHealth(releases: Vec<ReleaseHealthRow>) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300 mb-6",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base", "Release health" }
                div { class: "flex flex-col divide-y divide-base-300",
                    for release in releases.iter().filter(|r| r.sessions > 0) {
                        div { class: "flex items-center gap-4 py-2 first:pt-0 last:pb-0",
                            span { class: "font-mono text-sm truncate flex-1", "{release.version}" }
                            Sparkline {
                                counts: release.series.clone(),
                                unit: "sessions".to_string(),
                                fill: "fill-success/70".to_string(),
                            }
                            span { class: "text-sm text-base-content/60 tabular-nums w-24 text-right",
                                "{release.sessions} sessions"
                            }
                            match release.crash_free_rate {
                                Some(rate) => rsx! {
                                    span {
                                        class: if rate < 0.99 { "text-sm font-semibold tabular-nums w-28 text-right text-error" } else { "text-sm font-semibold tabular-nums w-28 text-right" },
                                        "{rate * 100.0:.2}% crash-free"
                                    }
                                },
                                // Below the floor a rate is noise dressed as a measurement: one
                                // crash in three sessions reads as a catastrophe and means nothing.
                                None => rsx! {
                                    span { class: "text-sm text-base-content/40 w-28 text-right",
                                        "not enough data"
                                    }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Cron monitors and whether their last run was on time.
///
/// A failed run also raises an ordinary issue, so this panel is a status board, not the alert
/// path — the issue list below is where a miss actually gets triaged.
#[component]
fn Monitors(monitors: Vec<MonitorRow>) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300 mb-6",
            div { class: "card-body gap-3",
                div { class: "text-xs uppercase tracking-wide text-base-content/50", "Cron monitors" }
                div { class: "flex flex-col gap-2",
                    for monitor in monitors {
                        div { class: "flex items-center justify-between gap-4 text-sm",
                            div { class: "min-w-0",
                                div { class: "font-medium truncate", "{monitor.slug}" }
                                div { class: "text-xs text-base-content/50 font-mono",
                                    "{monitor.schedule} · {monitor.timezone}"
                                }
                            }
                            div { class: "flex items-center gap-3 shrink-0",
                                if let Some(next) = &monitor.next_due_at {
                                    span { class: "text-xs text-base-content/50", "next {next}" }
                                }
                                span { class: "badge badge-sm {monitor_badge(monitor.status.as_deref())}",
                                    "{monitor.status.clone().unwrap_or_else(|| \"pending\".into())}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A missed or timed-out job is an outage, so it reads as an error; `error` is a run that ran and
/// reported failure, which is the job's own problem to report in detail.
fn monitor_badge(status: Option<&str>) -> &'static str {
    match status {
        Some("ok") => "badge-success",
        Some("missed") | Some("timeout") => "badge-error",
        Some("error") => "badge-warning",
        // Not `badge-ghost`: on a card it is the card's own colour, i.e. invisible.
        _ => "badge-neutral",
    }
}

/// One row. The checkbox sits beside the link rather than inside it: a click inside an anchor
/// navigates whatever else it does, and stopping that would also stop the box from toggling.
#[component]
fn IssueCard(
    row: IssueRow,
    selected: Signal<BTreeSet<i64>>,
    highlighted: bool,
    selectable: bool,
) -> Element {
    let badge = level_class(&row.level);
    let id = row.id;
    let checked = selected.read().contains(&id);
    let border = if checked {
        "border-primary/60"
    } else {
        "border-base-300 hover:border-primary/50"
    };
    let ring = if highlighted {
        "ring-2 ring-primary/50"
    } else {
        ""
    };

    rsx! {
        div {
            class: "card bg-base-200 border transition-colors {border} {ring}",
            "data-issue-id": "{id}",
            div { class: if selectable { "card-body py-3 pl-3 flex-row items-center gap-3" } else { "card-body py-3 flex-row items-center gap-3" },
                if selectable {
                    input {
                        r#type: "checkbox",
                        class: "checkbox checkbox-sm shrink-0",
                        "aria-label": "Select issue {id}",
                        checked,
                        onchange: move |_| {
                            let mut set = selected.write();
                            if !set.remove(&id) {
                                set.insert(id);
                            }
                        },
                    }
                }
                Link {
                    to: Route::IssueDetail { id },
                    class: "flex flex-1 items-center gap-4 min-w-0",
                div { class: "flex-1 min-w-0",
                    div { class: "flex items-center gap-2 flex-wrap",
                        span { class: "badge badge-sm {badge}", "{row.level}" }
                        if row.status != "unresolved" {
                            span { class: "badge badge-sm badge-neutral", "{row.status}" }
                        }
                        // Ties the header's "New" count to the rows it is counting.
                        if row.is_new {
                            span { class: "badge badge-sm badge-warning badge-outline", "new" }
                        }
                        // The triage loop, live: an agent holds this right now, or nobody has
                        // picked it up.
                        match row.triage.as_deref() {
                            Some("claimed") => rsx! {
                                span { class: "badge badge-sm badge-primary gap-1.5",
                                    span { class: "w-1.5 h-1.5 rounded-full bg-current animate-pulse" }
                                    "agent working"
                                }
                            },
                            Some("queued") => rsx! {
                                span { class: "badge badge-sm badge-neutral", "awaiting triage" }
                            },
                            _ => rsx! {},
                        }
                        // Signals that an agent already did the work of diagnosing this.
                        if row.has_analysis {
                            span { class: "badge badge-sm badge-primary badge-outline", "analysed" }
                        }
                    }
                    div { class: "font-medium truncate mt-1", "{row.title}" }
                    if let Some(culprit) = &row.culprit {
                        div { class: "text-xs text-base-content/50 truncate font-mono", "{culprit}" }
                    }
                }
                div { class: "hidden sm:block", Sparkline { counts: row.counts.clone() } }
                // "Is this still happening?" — the count alone cannot answer that.
                div { class: "text-right w-20 hidden lg:block",
                    div { class: "font-semibold tabular-nums whitespace-nowrap", "{row.last_seen_ago}" }
                    div { class: "text-xs text-base-content/50", "last seen" }
                }
                div { class: "text-right w-16",
                    div { class: "font-semibold tabular-nums", "{row.times_seen}" }
                    // "total", because the stats card above counts the selected window
                    // while this is the issue's lifetime count.
                    div { class: "text-xs text-base-content/50", "events total" }
                }
                // 10,000 events on one user and 500 events on 400 users are different problems.
                if row.users_affected > 0 {
                    div { class: "text-right w-14 hidden md:block",
                        div { class: "font-semibold tabular-nums", "{row.users_affected}" }
                        div { class: "text-xs text-base-content/50", "users" }
                    }
                }
                }
            }
        }
    }
}

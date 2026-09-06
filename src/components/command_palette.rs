//! ⌘K: jump to a project by name or to an issue by id from anywhere in the dashboard.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::LdSearch};

use crate::errors_data::list_projects;
use crate::models::errors::ProjectSummary;
use crate::routes::Route;

/// Opens on ⌘K / Ctrl+K from anywhere in the dashboard; Escape closes. Installed once for the
/// shell's lifetime and removed with it. Keys typed into fields are left to the fields — except
/// the shortcut itself, which should work from the search box too.
const KEY_LISTENER: &str = r#"
    window.__thermitePaletteKeys = (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
            e.preventDefault();
            dioxus.send('open');
        } else if (e.key === 'Escape') {
            dioxus.send('close');
        }
    };
    document.addEventListener('keydown', window.__thermitePaletteKeys);
"#;
const KEY_LISTENER_REMOVE: &str = r#"
    document.removeEventListener('keydown', window.__thermitePaletteKeys);
    delete window.__thermitePaletteKeys;
"#;

/// Most results shown at once. A palette is for jumping, not browsing.
const MAX_RESULTS: usize = 8;

/// Where a result leads.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Issue(i64),
    Project { slug: String, name: String },
}

impl Target {
    fn route(&self) -> Route {
        match self {
            Target::Issue(id) => Route::IssueDetail { id: *id },
            Target::Project { slug, .. } => Route::issues(slug.clone()),
        }
    }
}

/// An issue id typed as `123` or `#123`. Anything else is a project search.
fn issue_id(query: &str) -> Option<i64> {
    let digits = query.trim().trim_start_matches('#');
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

fn results(query: &str, projects: &[ProjectSummary]) -> Vec<Target> {
    let query = query.trim();
    let mut out = Vec::new();
    if let Some(id) = issue_id(query) {
        out.push(Target::Issue(id));
    }
    // The digits of an id are still worth matching against slugs and names.
    let needle = query.trim_start_matches('#').to_lowercase();
    out.extend(
        projects
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.name.to_lowercase().contains(&needle)
                    || p.slug.to_lowercase().contains(&needle)
            })
            .take(MAX_RESULTS)
            .map(|p| Target::Project {
                slug: p.slug.clone(),
                name: p.name.clone(),
            }),
    );
    out
}

#[component]
pub fn CommandPalette(open: Signal<bool>) -> Element {
    let mut query = use_signal(String::new);
    let mut highlight = use_signal(|| 0usize);
    // Fetched the first time the palette opens and kept: a self-hosted instance has few
    // projects, and they rarely change mid-session.
    let mut projects = use_signal(|| None::<Vec<ProjectSummary>>);
    let nav = use_navigator();

    use_future(move || async move {
        if !cfg!(feature = "web") {
            return;
        }
        let mut keys = document::eval(KEY_LISTENER);
        while let Ok(key) = keys.recv::<String>().await {
            match key.as_str() {
                "open" => open.set(true),
                "close" => open.set(false),
                _ => {}
            }
        }
    });
    use_drop(move || {
        if cfg!(feature = "web") {
            let _ = document::eval(KEY_LISTENER_REMOVE);
        }
    });

    // Reset on every open, and load the projects once.
    use_effect(move || {
        if open() {
            query.set(String::new());
            highlight.set(0);
            if projects.peek().is_none() {
                spawn(async move {
                    if let Ok(list) = list_projects().await {
                        projects.set(Some(list));
                    }
                });
            }
        }
    });

    let mut go = move |target: Target| {
        open.set(false);
        nav.push(target.route());
    };

    if !open() {
        return rsx! {};
    }

    let found = results(&query(), projects.read().as_deref().unwrap_or_default());
    let count = found.len();

    rsx! {
        div {
            class: "modal modal-open",
            onclick: move |_| open.set(false),
            div {
                class: "modal-box p-0 max-w-lg overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                label { class: "flex items-center gap-2 px-4 py-3 border-b border-base-300",
                    Icon { icon: LdSearch, width: 16, height: 16, class: "opacity-50" }
                    input {
                        class: "grow bg-transparent outline-none text-sm",
                        placeholder: "Jump to a project, or an issue id like #42",
                        value: "{query}",
                        onmounted: move |e| async move {
                            let _ = e.set_focus(true).await;
                        },
                        oninput: move |e| {
                            query.set(e.value());
                            highlight.set(0);
                        },
                        onkeydown: {
                            let found = found.clone();
                            move |e| match e.key() {
                                Key::ArrowDown => {
                                    e.prevent_default();
                                    highlight.set((highlight() + 1).min(count.saturating_sub(1)));
                                }
                                Key::ArrowUp => {
                                    e.prevent_default();
                                    highlight.set(highlight().saturating_sub(1));
                                }
                                Key::Enter => {
                                    if let Some(target) = found.get(highlight()) {
                                        go(target.clone());
                                    }
                                }
                                _ => {}
                            }
                        },
                    }
                    kbd { class: "kbd kbd-xs", "esc" }
                }
                if found.is_empty() {
                    div { class: "px-4 py-6 text-sm text-base-content/50 text-center",
                        if projects.read().is_none() {
                            span { class: "loading loading-spinner loading-xs" }
                        } else {
                            "No project matches. Type an issue id to open it directly."
                        }
                    }
                } else {
                    ul { class: "menu p-2",
                        for (i , target) in found.iter().cloned().enumerate() {
                            li {
                                a {
                                    class: if i == highlight() { "active" } else { "" },
                                    onmouseenter: move |_| highlight.set(i),
                                    onclick: {
                                        let target = target.clone();
                                        move |_| go(target.clone())
                                    },
                                    match &target {
                                        Target::Issue(id) => rsx! {
                                            span { class: "font-mono", "#{id}" }
                                            span { class: "text-base-content/60", "open this issue" }
                                        },
                                        Target::Project { slug, name } => rsx! {
                                            span { "{name}" }
                                            span { class: "font-mono text-xs text-base-content/50", "{slug}" }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "flex gap-3 px-4 py-2 border-t border-base-300 text-xs text-base-content/40",
                    span { kbd { class: "kbd kbd-xs", "↑" } " " kbd { class: "kbd kbd-xs", "↓" } " move" }
                    span { kbd { class: "kbd kbd-xs", "↵" } " open" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(slug: &str, name: &str) -> ProjectSummary {
        ProjectSummary {
            id: 1,
            slug: slug.into(),
            name: name.into(),
            dsn: String::new(),
            unresolved_issues: 0,
            total_issues: 0,
            events_last_24h: 0,
            alert_email: None,
            alert_webhook: None,
            repo_url: None,
            keys: Vec::new(),
        }
    }

    #[test]
    fn an_id_opens_the_issue_and_still_searches_projects() {
        let projects = [project("api-42", "API"), project("web", "Web")];
        let found = results("#42", &projects);
        assert_eq!(found[0], Target::Issue(42));
        assert!(matches!(&found[1], Target::Project { slug, .. } if slug == "api-42"));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn text_matches_name_or_slug_case_insensitively() {
        let projects = [
            project("checkout-api", "Checkout API"),
            project("web", "Storefront"),
        ];
        assert_eq!(results("CHECK", &projects).len(), 1);
        assert_eq!(results("store", &projects).len(), 1);
        assert_eq!(results("", &projects).len(), 2);
        assert!(results("nothing", &projects).is_empty());
    }
}

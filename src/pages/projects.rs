//! Projects overview, and the form that mints a DSN.

use dioxus::prelude::*;

use dioxus_free_icons::{Icon, icons::ld_icons::LdSettings};

use crate::components::copy_dsn::CopyDsn;
use crate::components::toast::{ToastLevel, show_toast};
use crate::errors_data::{create_project, list_projects};
use crate::models::errors::{ProjectSummary, thousands};
use crate::routes::Route;
use crate::version::load_error;

/// The slug the server would accept for a display name: lowercase, with every run of characters
/// outside `[a-z0-9_]` collapsed to one `-`.
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[component]
pub fn Projects() -> Element {
    let projects = use_resource(move || async move { list_projects().await });
    let mut name = use_signal(String::new);
    let mut slug = use_signal(String::new);
    // The slug follows the name until it is typed into, then it is the user's.
    let mut slug_edited = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let nav = use_navigator();

    let submit = move || async move {
        if slug().trim().is_empty() {
            return;
        }
        creating.set(true);
        let result = create_project(slug().trim().to_string(), name().trim().to_string()).await;
        creating.set(false);

        match result {
            // Straight into the setup screen: a project with no events renders the SDK
            // instructions in place of an empty board, which is the next thing to do anyway.
            Ok(project) => {
                show_toast(format!("Created {}", project.slug), ToastLevel::Success);
                nav.push(Route::issues(project.slug));
            }
            Err(e) => show_toast(format!("Could not create project: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "max-w-4xl",
            h1 { class: "text-2xl font-bold mb-1", "Projects" }
            p { class: "text-base-content/60 mb-6",
                "Each project has its own DSN. Point an SDK at one and its errors land here."
            }

            div { class: "card bg-base-200 border border-base-300 mb-6",
                div { class: "card-body gap-3",
                    div { class: "text-sm font-medium", "New project" }
                    form {
                        class: "flex flex-wrap gap-2",
                        onsubmit: move |e| {
                            e.prevent_default();
                            async move { submit().await }
                        },
                        label { class: "flex flex-col flex-1 min-w-40",
                            span { class: "text-xs mb-1", "Name" }
                            input {
                                class: "input input-sm w-full",
                                placeholder: "Checkout API",
                                value: "{name}",
                                oninput: move |e| {
                                    let v = e.value();
                                    if !slug_edited() {
                                        slug.set(slugify(&v));
                                    }
                                    name.set(v);
                                },
                            }
                        }
                        label { class: "flex flex-col flex-1 min-w-40",
                            span { class: "text-xs mb-1", "Slug" }
                            input {
                                class: "input input-sm w-full font-mono",
                                placeholder: "letters, digits, - and _",
                                value: "{slug}",
                                oninput: move |e| {
                                    slug_edited.set(true);
                                    slug.set(e.value());
                                },
                            }
                        }
                        button {
                            class: "btn btn-sm btn-primary self-end",
                            r#type: "submit",
                            disabled: creating() || slug().trim().is_empty(),
                            if creating() {
                                span { class: "loading loading-spinner loading-xs" }
                            }
                            "Create"
                        }
                    }
                }
            }

            match &*projects.read_unchecked() {
                Some(Ok(list)) if list.is_empty() => rsx! {
                    div { class: "card bg-base-200 border border-base-300",
                        div { class: "card-body items-center text-center py-12",
                            p { class: "text-base-content/60", "No projects yet. Create one above." }
                        }
                    }
                },
                Some(Ok(list)) => rsx! {
                    div { class: "flex flex-col gap-3",
                        for project in list.iter().cloned() {
                            ProjectCard { project }
                        }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error", {load_error("Could not load projects", e)} }
                },
                None => rsx! {
                    div { class: "flex flex-col gap-3",
                        for _ in 0..2 {
                            div { class: "skeleton h-28" }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn ProjectCard(project: ProjectSummary) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                div { class: "flex items-center justify-between gap-4",
                    div {
                        Link {
                            to: Route::issues(project.slug.clone()),
                            class: "font-semibold hover:text-primary",
                            "{project.name}"
                        }
                        div { class: "text-xs text-base-content/50 font-mono", "{project.slug}" }
                    }
                    // Same order as the dashboard card: events then unresolved. They were
                    // reversed, so the same two numbers swapped places between the two
                    // pages the gear icon navigates between.
                    div { class: "flex items-center gap-6 text-right",
                        div { class: "w-16",
                            div { class: "text-lg font-semibold tabular-nums",
                                "{thousands(project.events_last_24h)}"
                            }
                            div { class: "text-xs text-base-content/50", "events 24h" }
                        }
                        div { class: "w-16",
                            div { class: "text-lg font-semibold tabular-nums",
                                "{thousands(project.unresolved_issues)}"
                            }
                            div { class: "text-xs text-base-content/50", "unresolved" }
                        }
                        Link {
                            to: Route::ProjectSettings { slug: project.slug.clone() },
                            class: "btn btn-sm btn-ghost btn-square shrink-0",
                            "aria-label": "Settings for {project.name}",
                            Icon { icon: LdSettings, width: 16, height: 16 }
                        }
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
                // Labeled component DSNs, read-only — minting and revoking live in settings.
                if !project.keys.is_empty() {
                    div {
                        div { class: "text-xs uppercase tracking-wide text-base-content/50 mb-1",
                            "Component DSNs"
                        }
                        div { class: "flex flex-col gap-1.5",
                            for key in project.keys.iter().cloned() {
                                div { class: "flex items-center gap-2",
                                    span { class: "badge badge-neutral badge-sm font-mono shrink-0 w-28 justify-start truncate", "{key.label}" }
                                    code { class: "flex-1 bg-base-300 rounded px-3 py-1.5 text-xs break-all font-mono",
                                        "{key.dsn}"
                                    }
                                    CopyDsn { dsn: key.dsn.clone(), label: key.label.clone() }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("Checkout API"), "checkout-api");
        assert_eq!(slugify("  my_worker (v2)!  "), "my_worker-v2");
        assert_eq!(slugify("---"), "");
        assert_eq!(slugify("Ünïcode"), "n-code");
    }
}

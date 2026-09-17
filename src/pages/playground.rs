//! A page for raising synthetic errors, so a fresh instance can be filled with realistic data and
//! its ingest path verified end to end.
//!
//! The buttons do not throw inside this process — they post real envelopes to this instance's own
//! ingest endpoint using the selected project's DSN, exactly as an SDK would. So a green result here
//! means DSN authentication, envelope parsing, grouping and the quota check all work.

use dioxus::prelude::*;

use crate::components::toast::{ToastLevel, show_toast};
use crate::errors_data::{list_projects, raise_demo_error};
use crate::models::demo::DEMO_KINDS;
use crate::routes::Route;
use crate::version::load_error;

#[component]
pub fn Playground() -> Element {
    let projects = use_resource(move || async move { list_projects().await });

    let mut selected = use_signal(|| None::<i64>);
    let mut environment = use_signal(|| "production".to_string());
    let mut release = use_signal(|| "1.0.0".to_string());
    let mut burst = use_signal(|| 1u32);
    // Which kind is currently in flight, so only that button shows a spinner.
    let mut sending = use_signal(|| None::<String>);

    // Default to the first project once the list arrives, so the buttons are usable immediately.
    use_effect(move || {
        if selected().is_none()
            && let Some(Ok(list)) = &*projects.read_unchecked()
            && let Some(first) = list.first()
        {
            selected.set(Some(first.id));
        }
    });

    let raise = move |kind: String| async move {
        let Some(project_id) = selected() else {
            show_toast("Pick a project first", ToastLevel::Warning);
            return;
        };

        sending.set(Some(kind.clone()));
        let result =
            raise_demo_error(project_id, kind.clone(), environment(), release(), burst()).await;
        sending.set(None);

        match result {
            Ok(sent) => show_toast(
                format!(
                    "Sent {sent} event{} — open the project to see {}",
                    if sent == 1 { "" } else { "s" },
                    if sent == 1 { "it" } else { "them" },
                ),
                ToastLevel::Success,
            ),
            Err(e) => show_toast(format!("Could not raise the error: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "max-w-4xl mx-auto",
            h1 { class: "text-2xl font-bold mb-1", "Playground" }
            p { class: "text-base-content/60 mb-6",
                "Raise synthetic errors to fill a project with realistic data. Each button posts a "
                "real envelope to this instance's own ingest endpoint using the project's DSN, so a "
                "successful send also proves ingest works."
            }

            match &*projects.read_unchecked() {
                Some(Ok(list)) if list.is_empty() => rsx! {
                    div { class: "card bg-base-200 border border-base-300",
                        div { class: "card-body items-center text-center py-12 gap-3",
                            p { class: "text-base-content/60",
                                "No projects yet. A project's DSN is what these events are sent with."
                            }
                            Link { to: Route::Projects {}, class: "btn btn-sm btn-primary",
                                "Create a project"
                            }
                        }
                    }
                },
                Some(Ok(list)) => rsx! {
                    // ── What to send it as ──
                    div { class: "card bg-base-200 border border-base-300 mb-6",
                        div { class: "card-body gap-3",
                            div { class: "text-sm font-medium", "Send as" }
                            // Explicit `flex flex-col`, not `form-control`: DaisyUI 5 dropped
                            // that class, so the label and its field were laying themselves out
                            // side by side in this wrapping row — the label sat left of the box
                            // on every field the wrap did not happen to break.
                            div { class: "flex flex-wrap items-end gap-3",
                                label { class: "flex flex-col gap-1",
                                    span { class: "text-xs text-base-content/60", "Project" }
                                    select {
                                        class: "select select-sm min-w-48",
                                        value: selected().map(|id| id.to_string()).unwrap_or_default(),
                                        onchange: move |e| selected.set(e.value().parse().ok()),
                                        for project in list.iter() {
                                            option { value: "{project.id}", "{project.slug}" }
                                        }
                                    }
                                }
                                label { class: "flex flex-col gap-1",
                                    span { class: "text-xs text-base-content/60", "Environment" }
                                    input {
                                        class: "input input-sm w-40",
                                        value: "{environment}",
                                        oninput: move |e| environment.set(e.value()),
                                    }
                                }
                                label { class: "flex flex-col gap-1",
                                    span { class: "text-xs text-base-content/60", "Release" }
                                    input {
                                        class: "input input-sm w-40",
                                        value: "{release}",
                                        oninput: move |e| release.set(e.value()),
                                    }
                                }
                                label { class: "flex flex-col gap-1",
                                    span { class: "text-xs text-base-content/60", "Events per click" }
                                    input {
                                        r#type: "number",
                                        min: "1",
                                        max: "25",
                                        class: "input input-sm w-28",
                                        value: "{burst}",
                                        oninput: move |e| {
                                            burst.set(e.value().parse().unwrap_or(1).clamp(1, 25))
                                        },
                                    }
                                }
                            }
                            p { class: "text-xs text-base-content/50",
                                "Environment and release become searchable tags. Resolving an issue "
                                "\"in the next release\" is judged against the releases seen here."
                            }
                        }
                    }

                    // ── One row per kind ──
                    //
                    // A list, not a card grid: there is an odd number of kinds, so two columns
                    // left the last one beside a hole, and five cards of identical shape carried
                    // no more than five rows do in a third of the height. Outlined buttons
                    // because five solid primaries side by side rank nothing above anything.
                    div { class: "card bg-base-200 border border-base-300 overflow-hidden",
                        for (i , kind) in DEMO_KINDS.iter().enumerate() {
                            div {
                                class: if i == 0 { "flex items-center gap-4 px-4 py-3" } else { "flex items-center gap-4 px-4 py-3 border-t border-base-300" },
                                div { class: "flex-1 min-w-0",
                                    div { class: "font-medium text-sm", "{kind.label}" }
                                    p { class: "text-xs text-base-content/60 mt-0.5",
                                        "{kind.description}"
                                    }
                                }
                                button {
                                    class: "btn btn-sm btn-outline shrink-0",
                                    disabled: sending().is_some() || selected().is_none(),
                                    onclick: {
                                        let id = kind.id.to_string();
                                        move |_| raise(id.clone())
                                    },
                                    if sending().as_deref() == Some(kind.id) {
                                        span { class: "loading loading-spinner loading-xs" }
                                    }
                                    "Raise"
                                }
                            }
                        }
                    }

                    if let Some(project) = list.iter().find(|p| Some(p.id) == selected()) {
                        div { class: "mt-6",
                            Link {
                                to: Route::issues(project.slug.clone()),
                                class: "btn btn-sm btn-ghost",
                                "View {project.slug} issues →"
                            }
                        }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error", {load_error("Could not load projects", e)} }
                },
                None => rsx! {
                    div { class: "flex flex-col gap-3",
                        div { class: "skeleton h-28 w-full" }
                        div { class: "skeleton h-64 w-full" }
                    }
                },
            }
        }
    }
}

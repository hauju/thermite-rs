//! Dashboard landing page: every project at a glance, the ones needing attention first.

use dioxus::prelude::*;

use crate::components::logo::ThermiteMark;
use crate::components::sparkline::Sparkline;
use crate::errors_data::project_overview;
use crate::models::errors::{ProjectOverviewRow, thousands};
use crate::routes::Route;

#[component]
pub fn Dashboard() -> Element {
    let overview = use_resource(move || async move { project_overview().await });

    rsx! {
        div { class: "max-w-4xl",
            div { class: "flex items-center gap-3 mb-6",
                div { class: "w-11 h-11 rounded-xl bg-primary/10 border border-primary/20 flex items-center justify-center shrink-0",
                    ThermiteMark { size: 26 }
                }
                div {
                    h1 { class: "text-2xl font-bold leading-tight", "Dashboard" }
                    p { class: "text-base-content/60 text-sm",
                        "All projects at a glance. Anything that needs a look floats to the top."
                    }
                }
            }

            match &*overview.read_unchecked() {
                Some(Ok(rows)) if rows.is_empty() => rsx! {
                    div { class: "card bg-base-200 border border-base-300",
                        div { class: "card-body items-center text-center py-12 gap-2",
                            p { class: "text-base-content/60", "No projects yet." }
                            Link {
                                to: Route::Projects {},
                                class: "btn btn-sm btn-primary",
                                "Create your first project"
                            }
                        }
                    }
                },
                Some(Ok(rows)) => rsx! {
                    Totals { rows: rows.clone() }
                    ProjectList { rows: rows.clone() }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error", "Could not load the overview: {e}" }
                },
                None => rsx! {
                    div { class: "flex flex-col gap-3",
                        div { class: "skeleton h-20" }
                        for _ in 0..3 {
                            div { class: "skeleton h-16" }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn Totals(rows: Vec<ProjectOverviewRow>) -> Element {
    let events: i64 = rows.iter().map(|r| r.events_last_24h).sum();
    let unresolved: i64 = rows.iter().map(|r| r.unresolved_issues).sum();
    let attention = rows.iter().filter(|r| r.needs_attention()).count();

    rsx! {
        div { class: "grid grid-cols-2 md:grid-cols-4 gap-3 mb-6",
            StatCard { title: "Projects", value: rows.len().to_string() }
            StatCard { title: "Events (24h)", value: thousands(events) }
            StatCard { title: "Unresolved issues", value: thousands(unresolved) }
            StatCard {
                title: "Need attention",
                value: attention.to_string(),
                accent: attention > 0,
            }
        }
    }
}

#[component]
fn StatCard(title: &'static str, value: String, #[props(default = false)] accent: bool) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body p-4",
                div { class: "text-xs text-base-content/60", "{title}" }
                div {
                    class: if accent { "text-2xl font-bold mt-1 tabular-nums text-warning" } else { "text-2xl font-bold mt-1 tabular-nums" },
                    "{value}"
                }
            }
        }
    }
}

#[component]
fn ProjectList(rows: Vec<ProjectOverviewRow>) -> Element {
    let mut sorted = rows;
    // Attention first, then the busiest — the quiet tail is what you scroll past, not through.
    sorted.sort_by_key(|r| (!r.needs_attention(), -r.events_last_24h));

    rsx! {
        div { class: "flex flex-col gap-3",
            for row in sorted {
                OverviewCard { key: "{row.slug}", row }
            }
        }
    }
}

#[component]
fn OverviewCard(row: ProjectOverviewRow) -> Element {
    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body p-4 gap-2",
                div { class: "flex items-center justify-between gap-3 sm:gap-4",
                    div { class: "min-w-0",
                        Link {
                            to: Route::issues(row.slug.clone()),
                            class: "font-semibold hover:text-primary truncate block",
                            "{row.name}"
                        }
                        div { class: "text-xs text-base-content/50 font-mono truncate", "{row.slug}" }
                    }
                    // The right block is ~264px that cannot shrink, so on a phone it ate the
                    // name column until the slug truncated to five characters. The sparkline
                    // is the one part that carries no number, so it goes first — same trade
                    // the issue list already makes.
                    div { class: "flex items-center gap-4 sm:gap-6 shrink-0",
                        div { class: "hidden sm:block",
                            Sparkline { counts: row.series.clone() }
                        }
                        div { class: "text-right w-16",
                            div { class: "text-lg font-semibold tabular-nums",
                                "{thousands(row.events_last_24h)}"
                            }
                            div { class: "text-xs text-base-content/50", "events 24h" }
                        }
                        div { class: "text-right w-16",
                            div { class: "text-lg font-semibold tabular-nums",
                                "{thousands(row.unresolved_issues)}"
                            }
                            div { class: "text-xs text-base-content/50", "unresolved" }
                        }
                    }
                }
                if row.needs_attention() {
                    div { class: "flex flex-wrap gap-1.5",
                        if row.new_issues_24h > 0 {
                            span { class: "badge badge-warning badge-sm",
                                if row.new_issues_24h == 1 { "1 new issue (24h)" } else { "{row.new_issues_24h} new issues (24h)" }
                            }
                        }
                        if row.monitors_failing > 0 {
                            span { class: "badge badge-error badge-sm",
                                if row.monitors_failing == 1 { "1 cron monitor failing" } else { "{row.monitors_failing} cron monitors failing" }
                            }
                        }
                        if row.alerts_dead_lettered > 0 {
                            span { class: "badge badge-error badge-sm",
                                if row.alerts_dead_lettered == 1 { "1 alert undeliverable" } else { "{row.alerts_dead_lettered} alerts undeliverable" }
                            }
                        }
                    }
                }
            }
        }
    }
}

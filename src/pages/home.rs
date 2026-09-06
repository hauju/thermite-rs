use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::components::logo::ThermiteMark;
use crate::errors_data::demo_project;
use crate::routes::Route;

/// Landing page.
#[component]
pub fn Home() -> Element {
    let demo = use_resource(|| async { demo_project().await.ok().flatten() });
    rsx! {
        section { class: "relative overflow-hidden",
            // Ambient hero backdrop: soft azure glow + masked guideline grid.
            div { class: "landing-hero-glow" }
            div { class: "landing-hero-grid" }

            div { class: "container relative mx-auto px-4 pt-20 pb-16 max-w-4xl",
                // Hero
                div { class: "flex flex-col items-center text-center",
                    span { class: "landing-hero-rise inline-flex items-center gap-2 rounded-full border border-base-300 bg-base-200/60 px-3 py-1 text-xs font-medium text-base-content/70 mb-8",
                        Icon { icon: LdServer, width: 14, height: 14 }
                        "Sentry-compatible, self-hosted"
                    }

                    div { class: "landing-hero-rise hero-delay-1 inline-flex items-center gap-4 mb-10",
                        ThermiteMark { size: 72 }
                        span { class: "font-display text-6xl sm:text-7xl font-bold tracking-tight",
                            "Thermite"
                        }
                    }

                    h1 { class: "landing-hero-rise hero-delay-2 text-4xl sm:text-5xl font-black tracking-tight mb-5",
                        "The error tracker "
                        span { class: "landing-gradient-text", "your agent works in." }
                    }
                    p { class: "landing-hero-rise hero-delay-3 text-lg text-base-content/70 max-w-xl mb-9",
                        "Point an unmodified Sentry SDK at Thermite and errors group into issues in your Postgres. Over MCP, a coding agent claims each new issue, reads the same stack trace you see, and leaves its diagnosis on the issue page."
                    }

                    div { class: "landing-hero-rise hero-delay-4 flex flex-col sm:flex-row items-center gap-3",
                        Link {
                            to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                            class: "btn btn-primary btn-lg btn-strong rounded-xl gap-2",
                            "Get Started"
                            Icon { icon: LdArrowRight, width: 18, height: 18 }
                        }
                        // The live board, when this instance exposes one: the shortest path to
                        // "what does it actually look like".
                        if let Some(Some(slug)) = demo() {
                            Link {
                                to: Route::issues(slug),
                                class: "btn btn-outline btn-lg rounded-xl gap-2",
                                Icon { icon: LdEye, width: 18, height: 18 }
                                "See the live demo"
                            }
                        }
                        Link {
                            to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                            class: "btn btn-ghost btn-lg rounded-xl gap-2",
                            Icon { icon: LdBookOpen, width: 18, height: 18 }
                            "Read the docs"
                        }
                    }
                }

                // The triage loop — the part no other error tracker has.
                div { class: "w-full mt-20",
                    div { class: "text-center mb-8",
                        h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight",
                            "From crash to diagnosis"
                        }
                        p { class: "text-base-content/60 mt-2 max-w-xl mx-auto",
                            "Thermite never calls a model. It hands the work to your own coding agent — leased, so two agents never triage the same bug twice."
                        }
                    }
                    div { class: "grid grid-cols-1 md:grid-cols-3 gap-4",
                        StepCard {
                            number: "1",
                            title: "An exception lands",
                            description: "Any Sentry SDK reports it. Grouping folds an error storm into one issue and queues exactly one unit of triage work — in the same transaction, so nothing slips through.",
                        }
                        StepCard {
                            number: "2",
                            title: "Your agent claims it",
                            description: "Over MCP it gets the exception chain, every stack frame, breadcrumbs — and the release that crashed, so it diagnoses against the revision that actually broke.",
                        }
                        StepCard {
                            number: "3",
                            title: "The diagnosis comes back",
                            description: "Root cause and suggested fix land on the issue page, next to the alert that told you. You read the answer, not the stack trace.",
                        }
                    }
                }

                // Feature cards
                div { class: "grid grid-cols-1 md:grid-cols-3 gap-4 w-full mt-20",
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdBot, width: 22, height: 22 } },
                        title: "Triage over MCP",
                        description: "An MCP server serves issues, stack traces and tags to coding agents, which write their findings back onto the issue.",
                    }
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdPlug, width: 22, height: 22 } },
                        title: "Drop-in for Sentry SDKs",
                        description: "Point your existing DSN at Thermite. Same envelope and store endpoints, same SDKs, no code change.",
                    }
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdFingerprint, width: 22, height: 22 } },
                        title: "Grouping that holds",
                        description: "Fingerprints normalize ids, IPs and durations, so one bug stays one issue instead of a thousand.",
                    }
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdBellRing, width: 22, height: 22 } },
                        title: "Alerts per project",
                        description: "Email and webhook routing, per project. Delivery is at-least-once from an outbox, so no alert is dropped in silence.",
                    }
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdAlarmClock, width: 22, height: 22 } },
                        title: "Cron monitoring",
                        description: "Jobs check in on their schedule. A missed or overrunning run becomes an ordinary error event, grouped and alerted like any other.",
                    }
                    FeatureCard {
                        icon: rsx! { Icon { icon: LdHeartPulse, width: 22, height: 22 } },
                        title: "Release health",
                        description: "Crash-free rate per release, counted from SDK sessions, so a busy release does not read as a broken one.",
                    }
                }
            }
        }
    }
}

#[component]
fn StepCard(number: &'static str, title: &'static str, description: &'static str) -> Element {
    rsx! {
        div { class: "card card-elevated bg-base-200 h-full",
            div { class: "card-body gap-3",
                span { class: "inline-flex items-center justify-center w-11 h-11 rounded-xl bg-primary/10 text-primary font-display text-xl font-bold",
                    "{number}"
                }
                h3 { class: "card-title text-lg", "{title}" }
                p { class: "text-base-content/60 text-sm leading-relaxed", "{description}" }
            }
        }
    }
}

#[component]
fn FeatureCard(icon: Element, title: &'static str, description: &'static str) -> Element {
    rsx! {
        div { class: "card card-elevated card-hover bg-base-200 h-full",
            div { class: "card-body gap-3",
                span { class: "icon-animate inline-flex items-center justify-center w-11 h-11 rounded-xl bg-primary/10 text-primary",
                    {icon}
                }
                h3 { class: "card-title text-lg", "{title}" }
                p { class: "text-base-content/60 text-sm leading-relaxed", "{description}" }
            }
        }
    }
}

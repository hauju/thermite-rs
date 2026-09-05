use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::routes::Route;

/// Pricing page.
///
/// Priced by event volume rather than by seat: the thing reading Thermite is usually an agent, and
/// charging per human for that makes no sense. Every plan carries the whole product — the tiers
/// differ only in how many events they accept.
#[component]
pub fn Pricing() -> Element {
    rsx! {
        section { class: "relative overflow-hidden",
            div { class: "landing-hero-glow" }
            div { class: "landing-hero-grid" }

            div { class: "container relative mx-auto px-4 pt-20 pb-16 max-w-5xl",
                div { class: "flex flex-col items-center text-center mb-14",
                    h1 { class: "landing-hero-rise text-4xl sm:text-5xl font-black tracking-tight mb-5",
                        "Priced by events, "
                        span { class: "landing-gradient-text", "not by seats." }
                    }
                    p { class: "landing-hero-rise hero-delay-1 text-lg text-base-content/70 max-w-2xl",
                        "Your agent is not a seat. Every plan includes the full triage loop, the MCP server and the REST API — the only thing that changes is how many events you send."
                    }
                }

                div { class: "grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4",
                    PlanCard {
                        name: "Free",
                        price: "$0",
                        cadence: "forever",
                        tagline: "For one project.",
                        volume: "1,000 events / month",
                        volume_note: "About a side project, or a quiet first week.",
                        features: vec![
                            "1 project",
                            "30-day retention",
                            "MCP server and REST API",
                            "Email and webhook alerts",
                        ],
                        cta: "Start free",
                        featured: false,
                    }
                    PlanCard {
                        name: "Pro",
                        price: "$19",
                        cadence: "/month",
                        tagline: "For a product in production.",
                        volume: "100,000 events / month",
                        volume_note: "About a small SaaS having a bad month.",
                        features: vec![
                            "Unlimited projects",
                            "90-day retention",
                            "Component keys",
                            "Cron monitoring and release health",
                        ],
                        cta: "Start free",
                        featured: true,
                    }
                    PlanCard {
                        name: "Team",
                        price: "$49",
                        cadence: "/month",
                        tagline: "For a product with traffic.",
                        volume: "1,000,000 events / month",
                        volume_note: "About steady production traffic across several services.",
                        features: vec![
                            "Everything in Pro",
                            "90-day retention",
                            "Priority support",
                            "Help migrating off Sentry",
                        ],
                        cta: "Start free",
                        featured: false,
                    }
                    SelfHostedCard {}
                }

                // Nobody can estimate their own event volume, which is where pricing by volume
                // loses people. Turn the unknown into a reason to sign up rather than a reason to
                // leave — the dashboard answers it from the outcomes rollup within a day.
                p { class: "text-center text-sm text-base-content/60 mt-8",
                    "Not sure what you send? Start free — the dashboard shows your real volume within a day."
                }

                // Said plainly rather than in fine print. For a product whose pitch is that error
                // data never leaves your infrastructure, being caught overstating what ships costs
                // more than the signups it would buy.
                div { class: "mx-auto mt-6 max-w-xl rounded-xl border border-base-300 bg-base-200/40 px-5 py-3 text-center text-sm text-base-content/60",
                    "Pro and Team are not billable yet. Sign up now and you are on Free with the limits lifted — we will tell you before that changes."
                }

                // The questions that actually decide whether someone signs up.
                div { class: "w-full mt-24",
                    h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight text-center mb-10",
                        "Questions worth answering"
                    }
                    div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
                        FaqCard {
                            question: "What counts as an event?",
                            answer: "One error or message an SDK sends and Thermite stores. Retries of an event you already sent are deduplicated and cost nothing, and the item types Thermite accepts but does not store — sessions, transactions, client reports — are never charged.",
                        }
                        FaqCard {
                            question: "What happens when I hit the limit?",
                            answer: "Over-quota events are rejected with a 429 and a Retry-After, exactly as Sentry does, so your SDK backs off and retries instead of failing. Nothing is dropped in silence: every rejection is counted and shown on the dashboard.",
                        }
                        FaqCard {
                            question: "Can I move between hosted and self-hosted?",
                            answer: "Both run the same software, so the move is a DSN change in your SDK config. Self-hosting is AGPL-3.0 and free for any use, including commercially inside your own company.",
                        }
                        FaqCard {
                            question: "Does my error data train a model?",
                            answer: "Thermite never calls a model at all. It queues issues, and your own coding agent — running on your machine, under your API key — does the thinking and writes its diagnosis back.",
                        }
                    }
                }

                div { class: "mt-16 rounded-2xl border border-base-300 bg-base-200/60 p-8 text-center",
                    h2 { class: "text-xl font-bold tracking-tight mb-2", "Need something else?" }
                    p { class: "text-base-content/60 max-w-xl mx-auto mb-5",
                        "Higher volume, a commercial licence for a fork you cannot publish, or an invoice instead of a card — all fine. Say what you need."
                    }
                    a {
                        class: "btn btn-outline rounded-xl gap-2",
                        href: "mailto:mail@haukejung.de",
                        Icon { icon: LdMail, width: 16, height: 16 }
                        "mail@haukejung.de"
                    }
                }
            }
        }
    }
}

#[component]
fn PlanCard(
    name: &'static str,
    price: &'static str,
    cadence: &'static str,
    tagline: &'static str,
    volume: &'static str,
    volume_note: &'static str,
    features: Vec<&'static str>,
    cta: &'static str,
    featured: bool,
) -> Element {
    let card_class = if featured {
        "card card-elevated bg-base-200 h-full ring-2 ring-primary relative"
    } else {
        "card card-elevated bg-base-200 h-full"
    };

    rsx! {
        div { class: "{card_class}",
            if featured {
                span { class: "absolute -top-3 left-1/2 -translate-x-1/2 rounded-full bg-primary px-3 py-1 text-xs font-semibold text-primary-content",
                    "Most popular"
                }
            }
            div { class: "card-body gap-4",
                div {
                    h3 { class: "font-display text-lg font-bold", "{name}" }
                    p { class: "text-base-content/60 text-sm mt-1", "{tagline}" }
                }
                div { class: "flex items-baseline gap-1",
                    span { class: "font-display text-4xl font-black tracking-tight", "{price}" }
                    span { class: "text-base-content/50 text-sm", "{cadence}" }
                }
                // The metered dimension sits with the price, not in the checklist: it is the thing
                // being bought, and the anchor under it is what lets a reader place themselves.
                div { class: "border-y border-base-300 py-3",
                    p { class: "text-sm font-semibold", "{volume}" }
                    p { class: "text-xs text-base-content/50 mt-0.5", "{volume_note}" }
                }
                ul { class: "flex flex-col gap-2",
                    for feature in features {
                        li { key: "{feature}", class: "flex items-start gap-2 text-sm text-base-content/70",
                            span { class: "text-primary mt-0.5 shrink-0",
                                Icon { icon: LdCheck, width: 15, height: 15 }
                            }
                            "{feature}"
                        }
                    }
                }
                div { class: "flex-1" }
                Link {
                    to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                    class: if featured { "btn btn-primary btn-strong rounded-xl w-full" } else { "btn btn-outline rounded-xl w-full" },
                    "{cta}"
                }
            }
        }
    }
}

/// Broken out from [`PlanCard`] because its call to action is documentation rather than signup —
/// there is nothing to buy, which is the point of the card.
#[component]
fn SelfHostedCard() -> Element {
    rsx! {
        div { class: "card card-elevated bg-base-200 h-full",
            div { class: "card-body gap-4",
                div {
                    h3 { class: "font-display text-lg font-bold", "Self-hosted" }
                    p { class: "text-base-content/60 text-sm mt-1", "Run it yourself. AGPL-3.0." }
                }
                div { class: "flex items-baseline gap-1",
                    span { class: "font-display text-4xl font-black tracking-tight", "$0" }
                    span { class: "text-base-content/50 text-sm", "forever" }
                }
                div { class: "border-y border-base-300 py-3",
                    p { class: "text-sm font-semibold", "Unlimited events" }
                    p { class: "text-xs text-base-content/50 mt-0.5",
                        "Bounded by your disk, not by us."
                    }
                }
                ul { class: "flex flex-col gap-2",
                    for feature in [
                        "Unlimited projects",
                        "Retention you set yourself",
                        "Every feature the hosted plans have",
                        "One binary and a Postgres",
                        "Your error data never leaves",
                    ] {
                        li { key: "{feature}", class: "flex items-start gap-2 text-sm text-base-content/70",
                            span { class: "text-primary mt-0.5 shrink-0",
                                Icon { icon: LdCheck, width: 15, height: 15 }
                            }
                            "{feature}"
                        }
                    }
                }
                div { class: "flex-1" }
                Link {
                    to: Route::DocsPage {
                        slug: vec!["getting-started".into(), "installation".into()],
                    },
                    class: "btn btn-outline rounded-xl w-full gap-2",
                    Icon { icon: LdBookOpen, width: 16, height: 16 }
                    "Read the docs"
                }
            }
        }
    }
}

#[component]
fn FaqCard(question: &'static str, answer: &'static str) -> Element {
    rsx! {
        div { class: "card card-elevated bg-base-200 h-full",
            div { class: "card-body gap-2",
                h3 { class: "card-title text-base", "{question}" }
                p { class: "text-base-content/60 text-sm leading-relaxed", "{answer}" }
            }
        }
    }
}

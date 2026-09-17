use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::components::faq::FaqCard;
use crate::components::meta::{JsonLd, PageMeta, offer, software_application};
use crate::routes::Route;
use crate::waitlist::{WaitlistForm, waitlist_open};

const DESCRIPTION: &str = "Priced by errors, not by seats. Every plan includes the full triage loop, the MCP server and the REST API; only the volume you send changes.";

/// The volumes the slider snaps to. Tiered pricing gets a tiered control: a continuous slider
/// would promise a price for 37,000 errors that no plan has.
const STOPS: [&str; 4] = ["1k", "100k", "1M", "More"];

/// The volume a stop stands for, as the number a reader compares with their own.
fn stop_label(stop: usize) -> &'static str {
    match stop {
        0 => "1,000",
        1 => "100,000",
        2 => "1,000,000",
        _ => "1,000,000+",
    }
}

/// Pricing page.
///
/// Priced by error volume rather than by seat: the thing reading Thermite is usually an agent, and
/// charging per human for that makes no sense. Every plan carries the whole product — the tiers
/// differ only in how many errors they accept, so the page leads with that one number: a slider
/// sets it and the plan that fits lights up.
#[component]
pub fn Pricing() -> Element {
    // Starts on the middle plan, so a reader who never touches the slider still sees a
    // recommendation rather than the free tier.
    let mut selected = use_signal(|| 1usize);
    // Server-rendered so the calls to action do not flip after the first paint.
    let waitlist = use_server_future(|| async { waitlist_open().await.unwrap_or(false) })?;
    let waitlist = waitlist() == Some(true);

    // Past the last tier the Team card stays the answer and becomes a conversation. Plain Rust
    // here rather than string literals inside the props: rsx formats those into `String`s.
    let beyond = selected() == 3;
    let team_price = if beyond { "$49+" } else { "$49" };
    let team_volume = if beyond {
        "More than a million errors / month"
    } else {
        "1,000,000 errors / month"
    };
    let team_note = if beyond {
        "Priced by what you send. Tell us the number and we quote it."
    } else {
        "About steady production traffic across several services."
    };
    let team_cta = if beyond { "Talk to us" } else { "Start free" };

    rsx! {
        PageMeta {
            title: "Pricing — Thermite",
            description: DESCRIPTION,
            path: "/pricing",
        }
        // The same three plans as the cards below; keep the two in step.
        JsonLd {
            data: software_application("/pricing", DESCRIPTION, serde_json::json!([
                offer("Free", "0", "1,000 errors / month"),
                offer("Pro", "19", "100,000 errors / month"),
                offer("Team", "49", "1,000,000 errors / month"),
            ])),
        }
        section { class: "relative overflow-hidden",
            div { class: "landing-hero-glow" }
            div { class: "landing-hero-grid" }

            div { class: "container relative mx-auto px-4 pt-20 pb-16 max-w-5xl",
                div { class: "flex flex-col items-center text-center mb-10",
                    h1 { class: "landing-hero-rise text-4xl sm:text-5xl font-black tracking-tight mb-5",
                        "Priced by errors, "
                        span { class: "landing-gradient-text", "not by seats." }
                    }
                    p { class: "landing-hero-rise hero-delay-1 text-lg text-base-content/70 max-w-2xl",
                        "Your agent is not a seat. Every plan includes the full triage loop, the MCP server and the REST API — the only thing that changes is how many errors you send."
                    }
                }

                // The one number that picks a plan. The stop drives the cards below: the plan
                // it lands on gets the ring. Past the last tier the Team card keeps it and turns
                // into a conversation — that reader is the most valuable one on the page, and
                // pointing them at self-hosting would send them away for nothing.
                div { class: "card card-elevated bg-base-200 mb-6",
                    div { class: "card-body gap-3",
                        div { class: "flex flex-wrap items-start justify-between gap-x-6 gap-y-1",
                            div {
                                p { class: "text-sm font-semibold", "Errors a month" }
                                p { class: "text-xs text-base-content/50",
                                    "The same binary as self-hosted, run by us with the dashboard, MCP server and alerts attached. Set your volume."
                                }
                            }
                            span { class: "font-display text-2xl font-black tracking-tight tabular-nums",
                                "{stop_label(selected())}"
                            }
                        }
                        input {
                            r#type: "range",
                            min: "0",
                            max: "3",
                            step: "1",
                            value: "{selected}",
                            class: "range range-primary w-full",
                            aria_label: "Errors a month",
                            // The value is the index of a stop, so the control announced "1 of
                            // 3" — the one number on it that means nothing. This says what the
                            // handle is actually sitting on.
                            "aria-valuetext": "{stop_label(selected())} errors a month",
                            oninput: move |e| selected.set(e.value().parse().unwrap_or(1)),
                        }
                        div { class: "flex justify-between px-1 text-xs text-base-content/50",
                            for (i, stop) in STOPS.iter().enumerate() {
                                span {
                                    key: "{stop}",
                                    class: if selected() == i { "text-primary font-semibold" } else { "" },
                                    "{stop}"
                                }
                            }
                        }
                    }
                }

                div { class: "grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4",
                    PlanCard {
                        name: "Free",
                        price: "$0",
                        cadence: "forever",
                        tagline: "For one project.",
                        volume: "1,000 errors / month",
                        volume_note: "About a side project, or a quiet first week.",
                        features: vec![
                            "1 project",
                            "30-day retention",
                            "MCP server and REST API",
                            "Email and webhook alerts",
                        ],
                        cta: "Start free",
                        waitlist,
                        featured: selected() == 0,
                    }
                    PlanCard {
                        name: "Pro",
                        price: "$19",
                        cadence: "/month",
                        tagline: "For a product in production.",
                        volume: "100,000 errors / month",
                        volume_note: "About a small SaaS having a bad month.",
                        features: vec![
                            "Unlimited projects",
                            "90-day retention",
                            "Component keys",
                            "Cron monitoring and release health",
                        ],
                        cta: "Start free",
                        waitlist,
                        featured: selected() == 1,
                    }
                    PlanCard {
                        name: "Team",
                        price: team_price,
                        cadence: "/month",
                        tagline: "For a product with traffic.",
                        volume: team_volume,
                        volume_note: team_note,
                        features: vec![
                            "Everything in Pro",
                            "90-day retention",
                            "Priority support",
                            "Help migrating off Sentry",
                        ],
                        cta: team_cta,
                        waitlist,
                        contact: beyond,
                        featured: selected() >= 2,
                    }
                    SelfHostedCard {}
                }

                // Where the cards' "Join the waitlist" buttons point while hosted signup is closed.
                if waitlist {
                    div { id: "waitlist", class: "mt-10 flex flex-col items-center gap-3 text-center",
                        h2 { class: "text-xl font-bold tracking-tight", "Hosted signup opens in batches." }
                        p { class: "text-sm text-base-content/60 max-w-lg",
                            "Leave an address and you hear from us when yours comes up. Self-hosting needs no invitation, and the live demo needs no account."
                        }
                        WaitlistForm {}
                    }
                }

                // Nobody can estimate their own error volume, which is where pricing by volume
                // loses people. Turn the unknown into a reason to sign up rather than a reason to
                // leave — the dashboard answers it from the outcomes rollup within a day.
                p { class: "text-center text-sm text-base-content/60 mt-8",
                    if waitlist {
                        "Not sure what you send? Once you are in, the dashboard shows your real volume within a day."
                    } else {
                        "Not sure what you send? Start free — the dashboard shows your real volume within a day."
                    }
                }

                // Said plainly rather than in fine print. For a product whose pitch is that error
                // data never leaves your infrastructure, being caught overstating what ships costs
                // more than the signups it would buy.
                div { class: "mx-auto mt-6 max-w-xl rounded-xl border border-base-300 bg-base-200/40 px-5 py-3 text-center text-sm text-base-content/60",
                    if waitlist {
                        "Pro and Team are not billable yet, and nothing is charged before we say so — the waitlist is how you hear about it first."
                    } else {
                        "Pro and Team are not billable yet. Sign up now and you are on Free with the limits lifted — we will tell you before that changes."
                    }
                }

                // The questions that actually decide whether someone signs up.
                div { class: "w-full mt-24",
                    h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight text-center mb-10",
                        "Questions worth answering"
                    }
                    div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
                        FaqCard {
                            question: "What counts as an error?",
                            answer: "One error an SDK sends and Thermite stores — every occurrence, not every distinct bug. A storm of the same crash counts each time it happens, though it lands in one issue and, past your quota, is throttled rather than billed. Retries of an error you already sent are deduplicated and cost nothing, and what Thermite accepts but does not store — sessions, transactions, client reports — is never charged.",
                        }
                        FaqCard {
                            question: "What happens when I hit the limit?",
                            answer: "Over-quota errors are rejected with a 429 and a Retry-After, exactly as Sentry does, so your SDK backs off and retries instead of failing. Nothing is dropped in silence: every rejection is counted and shown on the dashboard.",
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
    /// The call to action is a conversation, not a signup.
    #[props(default)]
    contact: bool,
    /// Hosted signup is closed: the call to action points at the waitlist form instead.
    #[props(default)]
    waitlist: bool,
    featured: bool,
) -> Element {
    let card_class = if featured {
        "card card-elevated bg-base-200 h-full ring-2 ring-primary relative"
    } else {
        "card card-elevated bg-base-200 h-full"
    };
    let cta_class = if featured {
        "btn btn-primary btn-strong rounded-xl w-full"
    } else {
        "btn btn-outline rounded-xl w-full"
    };

    rsx! {
        div { class: "{card_class}",
            if featured {
                span { class: "absolute -top-3 left-1/2 -translate-x-1/2 rounded-full bg-primary px-3 py-1 text-xs font-semibold text-primary-content whitespace-nowrap",
                    "Your volume"
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
                if contact {
                    a { class: "{cta_class}", href: "mailto:mail@haukejung.de", "{cta}" }
                } else if waitlist {
                    a { class: "{cta_class}", href: "#waitlist", "Join the waitlist" }
                } else {
                    Link {
                        to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                        class: "{cta_class}",
                        "{cta}"
                    }
                }
            }
        }
    }
}

/// Broken out from [`PlanCard`] because its call to action is documentation rather than signup —
/// there is nothing to buy, which is the point of the card. The slider never lands on it: it is
/// the anchor that makes the hosted prices read as fair, not a plan to be steered into.
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
                    p { class: "text-sm font-semibold", "Unlimited errors" }
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

#[cfg(test)]
mod tests {
    use super::{STOPS, stop_label};

    #[test]
    fn every_stop_has_a_volume_and_the_last_is_open_ended() {
        let labels: Vec<_> = (0..STOPS.len()).map(stop_label).collect();
        assert_eq!(labels, ["1,000", "100,000", "1,000,000", "1,000,000+"]);
        // Anything the range input could send past the last stop reads as the open end.
        assert_eq!(stop_label(42), "1,000,000+");
    }
}

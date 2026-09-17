use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::components::faq::FaqCard;
use crate::components::logo::ThermiteMark;
use crate::components::meta::{
    JsonLd, PageMeta, SITE_DESCRIPTION, faq_page, offer, software_application,
};
use crate::errors_data::demo_link;
use crate::routes::Route;
use crate::waitlist::{WaitlistForm, waitlist_open};

/// The issue page of a real instance, captured at 1440x900 in the dark theme the product ships.
/// Its intrinsic size is on the tag so the hero does not reflow when it decodes.
const PRODUCT_SHOT: Asset = asset!("/assets/img/product-issue.webp");

/// The embers animation, as a deferred script in the server-rendered page so it starts with
/// the mark's CSS entrance, not seconds later when the WASM bundle has hydrated. The same
/// source runs from `onmounted` for client-side navigation, where a script element cloned
/// from a template does not execute; the guard keeps the two from restarting each other.
const HERO_EMBERS: Asset = asset!("/assets/hero-embers.js");
const HERO_EMBERS_ON_MOUNT: &str = concat!(
    "if (!window.__thermiteEmbersCleanup) {",
    include_str!("../../assets/hero-embers.js"),
    "}"
);

/// How Thermite compares with the two things a reader is already choosing between: label, then
/// Thermite, Sentry's hosted service and Sentry's self-hosted stack. Facts only — the stack and
/// the licence are what Sentry itself publishes.
const COMPARISON: [(&str, &str, &str, &str); 5] = [
    (
        "Ingest",
        "Any Sentry SDK",
        "Any Sentry SDK",
        "Any Sentry SDK",
    ),
    (
        "Runs on",
        "One binary + Postgres",
        "Their cloud",
        "Kafka, ClickHouse, Redis, Snuba, Relay and more",
    ),
    (
        "Priced by",
        "Errors sent",
        "Errors sent plus seats",
        "Your ops time",
    ),
    (
        "Agent triage over MCP",
        "Built in",
        "Not offered",
        "Not offered",
    ),
    ("License", "AGPL-3.0", "Proprietary", "FSL"),
];

/// The questions that decide whether someone switches, and their answers. One array, read by
/// both the cards and the `FAQPage` structured data, so the answer a search result shows is the
/// answer on the page.
const FAQ: [(&str, &str); 6] = [
    (
        "Do I have to change code?",
        "No. Point the DSN in your existing SDK config at Thermite. Same envelope and store endpoints, so the SDK cannot tell the difference.",
    ),
    (
        "Which SDKs work?",
        "Any Sentry SDK, unmodified: sentry-python, @sentry/browser, sentry-rust, and the rest. Services already on OpenTelemetry can send OTLP logs instead — ERROR and above become events.",
    ),
    (
        "Which agents can triage?",
        "Anything that speaks MCP: Claude Code, Cursor, Codex, or a script of your own. Claude Code connects with an API key; claude.ai connects over OAuth.",
    ),
    (
        "Does Thermite send my errors to a model?",
        "Never. It calls no model at all. It queues each new issue, and your own agent — running on your machine under your key — claims it over MCP and writes the diagnosis back.",
    ),
    (
        "What if I don't use agents yet?",
        "It is a complete error tracker on its own: grouping, alerts by email and webhook, cron monitoring, release health. The triage queue just waits.",
    ),
    (
        "What do I need to self-host?",
        "One binary and a Postgres. No Kafka, no ClickHouse, no dozen containers. AGPL-3.0, free for any use including inside your company.",
    ),
];

/// Landing page.
#[component]
pub fn Home() -> Element {
    // Server-rendered, like the waitlist flag: a button that appears once the client has
    // hydrated reads as a layout jump, not a feature.
    let demo = use_server_future(|| async { demo_link().await.ok().flatten() })?;
    use_drop(|| {
        let _ = document::eval("window.__thermiteEmbersCleanup?.();");
    });
    // Server-rendered rather than a skeleton: the primary call to action must not flip from a
    // button to a form after the page has painted.
    let waitlist = use_server_future(|| async { waitlist_open().await.unwrap_or(false) })?;
    let waitlist = waitlist() == Some(true);
    rsx! {
        PageMeta {
            title: "The error tracker your agent works in — Thermite",
            description: SITE_DESCRIPTION,
            path: "/",
        }
        JsonLd { data: software_application("/", SITE_DESCRIPTION, offer("Free", "0", "1,000 errors / month")) }
        JsonLd { data: faq_page(&FAQ) }
        section { class: "relative overflow-hidden",
            // Ambient hero backdrop: soft azure glow + masked guideline grid.
            div { class: "landing-hero-glow" }
            div { class: "landing-hero-grid" }
            // Embers off the mark, drawn beneath the copy (`assets/hero-embers.js`).
            canvas {
                id: "hero-embers",
                class: "landing-hero-embers",
                "aria-hidden": "true",
                onmounted: move |_| {
                    let _ = document::eval(HERO_EMBERS_ON_MOUNT);
                },
            }
            script { src: HERO_EMBERS, defer: true }

            div { class: "container relative mx-auto px-4 pt-20 pb-16 max-w-4xl",
                // Hero
                div { class: "flex flex-col items-center text-center",
                    div {
                        id: "hero-brand",
                        class: "landing-hero-rise hero-delay-1 inline-flex items-center gap-4 mb-10",
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
                        "Drop-in for any Sentry SDK, self-hosted on your Postgres. A coding agent picks up each new issue over MCP and leaves its diagnosis and a pull request on the issue page."
                    }

                    // While hosted signup is behind the waitlist, the form takes the primary spot:
                    // a "Get Started" that ends at a closed registration is worse than none.
                    if waitlist {
                        div { class: "landing-hero-rise hero-delay-4 w-full flex justify-center mb-4",
                            WaitlistForm {}
                        }
                    }
                    div { class: "landing-hero-rise hero-delay-4 flex flex-col sm:flex-row items-center gap-3",
                        if !waitlist {
                            Link {
                                to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                                class: "btn btn-primary btn-lg btn-strong rounded-xl gap-2",
                                "Get Started"
                                Icon { icon: LdArrowRight, width: 18, height: 18 }
                            }
                        }
                        // The live board, when this instance exposes one: the shortest path to
                        // "what does it actually look like".
                        if let Some(Some(url)) = demo() {
                            a {
                                href: "{url}",
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

                    // The whole migration, shown rather than claimed: a reader can hold this
                    // against their own config without opening the docs.
                    pre { class: "landing-hero-rise hero-delay-4 mt-10 w-full max-w-xl overflow-x-auto rounded-xl border border-base-300 bg-base-200 px-5 py-4 text-left font-mono text-sm",
                        code {
                            span { class: "text-base-content/40", "# the only change" }
                            "\nSENTRY_DSN=https://<key>@thermite.rs/1"
                        }
                    }
                }

                // What the copy above is describing, before any of the explaining starts: the
                // page a reader would land on, with an agent's findings already on it.
                div { class: "w-full mt-16",
                    p { class: "text-center text-sm text-base-content/60 mb-4",
                        "The crash, the agent's diagnosis, and the pull request that fixes it."
                    }
                    // After looking at a screenshot people try to click it, so it opens the
                    // live board when there is one, and says so underneath.
                    a {
                        href: demo().flatten(),
                        class: "block rounded-xl border border-base-300 shadow-2xl overflow-hidden bg-base-200",
                        img {
                            src: PRODUCT_SHOT,
                            width: "1440",
                            height: "900",
                            loading: "lazy",
                            decoding: "async",
                            class: "block w-full h-auto",
                            alt: "Thermite's issue page: a TypeError from checkout/pricing.py, an analysis posted by claude-code with high confidence against release 1.1.0, a Review the fix button linking to a pull request, and tag distributions for environment, release and server.",
                        }
                    }
                    if let Some(Some(url)) = demo() {
                        p { class: "text-center mt-4",
                            a {
                                href: "{url}",
                                class: "link link-primary inline-flex items-center gap-1 text-sm",
                                "Open this in the live demo"
                                Icon { icon: LdArrowRight, width: 14, height: 14 }
                            }
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
                            description: "Over MCP it gets the exception chain, every stack frame, breadcrumbs — and the release that crashed, so it diagnoses against the revision that actually broke. Claude Code, Cursor, Codex, or anything that speaks MCP.",
                        }
                        StepCard {
                            number: "3",
                            title: "The diagnosis comes back",
                            description: "Root cause and suggested fix land on the issue page, next to the alert that told you. You read the answer, not the stack trace.",
                        }
                    }
                }

                // Feature cards. The heading matters: without it this grid began immediately
                // after the three-step grid above, and six cards of the same shape read as a
                // continuation of the loop rather than as what the rest of the product is.
                div { class: "w-full mt-20",
                    div { class: "text-center mb-8",
                        h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight",
                            "And the rest of an error tracker"
                        }
                        p { class: "text-base-content/60 mt-2 max-w-xl mx-auto",
                            "The parts you would leave Sentry for, not just the part that is new."
                        }
                    }
                div { class: "grid grid-cols-1 md:grid-cols-3 gap-4 w-full",
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

                // The reader is already on Sentry, so the honest framing is what changes rather
                // than what is wrong with it: the wire protocol is the same on all three columns.
                div { class: "w-full mt-20",
                    div { class: "text-center mb-8",
                        h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight",
                            "Sentry, without the parts you were paying for"
                        }
                        p { class: "text-base-content/60 mt-2 max-w-xl mx-auto",
                            "Your SDKs stay exactly where they are. What changes is what it takes to run and what it is priced on."
                        }
                    }
                    // Scrolls sideways rather than wrapping: four columns of prose do not fold
                    // into a phone, and a broken table is harder to read than a scrolled one.
                    div { class: "overflow-x-auto rounded-xl border border-base-300 bg-base-200",
                        table { class: "table",
                            thead {
                                tr {
                                    th { }
                                    th { "Thermite" }
                                    th { "Sentry SaaS" }
                                    th { "Sentry self-hosted" }
                                }
                            }
                            tbody {
                                for (label , thermite , saas , self_hosted) in COMPARISON {
                                    tr { key: "{label}",
                                        th { class: "font-medium whitespace-nowrap", "{label}" }
                                        td { "{thermite}" }
                                        td { class: "text-base-content/60", "{saas}" }
                                        td { class: "text-base-content/60", "{self_hosted}" }
                                    }
                                }
                            }
                        }
                    }
                }

                // The questions that come up before a switch, answered on the page rather than
                // in a support thread.
                div { class: "w-full mt-20",
                    h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight text-center mb-8",
                        "Questions before you switch"
                    }
                    div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
                        for (question , answer) in FAQ {
                            FaqCard { key: "{question}", question, answer }
                        }
                    }
                }

                // The same two actions as the hero, for the reader who got this far: by here
                // they have the answer they came for and should not have to scroll back up.
                div { class: "w-full mt-20 rounded-2xl border border-base-300 bg-base-200/60 p-8 text-center",
                    h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight mb-2",
                        "Point a DSN at it."
                    }
                    p { class: "text-base-content/60 mb-6",
                        "Free for 1,000 errors a month, self-hosted for nothing."
                    }
                    if waitlist {
                        div { class: "flex justify-center mb-4",
                            WaitlistForm {}
                        }
                    }
                    div { class: "flex flex-col sm:flex-row items-center justify-center gap-3",
                        if !waitlist {
                            Link {
                                to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                                class: "btn btn-primary btn-lg btn-strong rounded-xl gap-2",
                                "Get Started"
                                Icon { icon: LdArrowRight, width: 18, height: 18 }
                            }
                        }
                        if let Some(Some(url)) = demo() {
                            a {
                                href: "{url}",
                                class: "btn btn-outline btn-lg rounded-xl gap-2",
                                Icon { icon: LdEye, width: 18, height: 18 }
                                "See the live demo"
                            }
                        }
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

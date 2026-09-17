use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::components::logo::ThermiteMark;
use crate::components::meta::{JsonLd, PageMeta, person};
use crate::routes::Route;

const HAUJU_IMG: Asset = asset!("/assets/img/hauju.jpeg");

const DESCRIPTION: &str = "Thermite is built by Hauke Jung, a full-stack developer in Germany, for teams whose coding agent reads the stack trace. The name, the mark, and why it is Rust all the way down.";

/// About page: who builds Thermite, where the name comes from, and what it stands for. Adapted
/// from seggwat.com/about — the same founder, the same shape, a different reaction.
#[component]
pub fn About() -> Element {
    rsx! {
        PageMeta {
            title: "About Thermite — Meet the founder",
            description: DESCRIPTION,
            path: "/about",
        }
        JsonLd { data: person("/about") }

        // Hero
        section { class: "container mx-auto px-4 pt-16 pb-16 max-w-6xl",
            div { class: "grid lg:grid-cols-2 gap-12 items-center",
                div { class: "space-y-6",
                    div { class: "landing-hero-rise flex items-center gap-3",
                        div { class: "w-16 h-0.5 bg-primary" }
                        span { class: "text-lg font-medium", "Meet the founder" }
                    }
                    h1 { class: "landing-hero-rise hero-delay-1 text-4xl lg:text-5xl font-black tracking-tight leading-tight",
                        "Hi, I'm "
                        span { class: "landing-gradient-text", "Hauke Jung" }
                        " — a full-stack developer who built Thermite because error trackers were built for a world where a human reads every stack trace."
                    }
                    p { class: "landing-hero-rise hero-delay-2 text-lg text-base-content/70 leading-relaxed",
                        "I spent years watching Sentry issues pile up: hundreds unread, alerts everyone had muted, and a bill that grew with our traffic for a tool one person ever opened. Then coding agents got good enough to read a stack trace, and the tracker was the thing in the way. Thermite is my answer: Sentry's wire protocol, one binary and a Postgres, and an MCP server that hands every new issue to your own agent — AGPL, so you can run it wherever you like."
                    }
                    div { class: "landing-hero-rise hero-delay-3 flex flex-wrap gap-3 pt-4",
                        a {
                            class: "btn btn-primary btn-strong rounded-xl gap-2",
                            href: "mailto:mail@haukejung.de",
                            Icon { icon: LdMail, width: 16, height: 16 }
                            "Get in contact"
                        }
                        Link {
                            to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                            class: "btn btn-outline rounded-xl gap-2",
                            Icon { icon: LdBookOpen, width: 16, height: 16 }
                            "Read the docs"
                        }
                        Link {
                            to: Route::Pricing {},
                            class: "btn btn-outline rounded-xl",
                            "Pricing"
                        }
                    }
                }
                div { class: "relative rounded-3xl overflow-hidden aspect-square",
                    img {
                        class: "w-full h-full object-cover",
                        src: HAUJU_IMG,
                        alt: "Hauke Jung, the founder of Thermite",
                        width: "1084",
                        height: "1200",
                    }
                    div { class: "absolute bottom-6 right-6 rounded-full border border-base-300 bg-base-200/90 px-6 py-2 shadow-lg backdrop-blur",
                        p { class: "font-display font-semibold text-lg leading-tight", "Hauke Jung" }
                        p { class: "text-xs text-base-content/60", "Thermite founder" }
                    }
                }
            }
        }

        NameStory {}
        WhyThermite {}
        GetInTouch {}
    }
}

/// The name and the mark. Goldschmidt's reaction is the metaphor the whole product is named
/// after, and the pun on the language is the part nobody needs explained.
#[component]
fn NameStory() -> Element {
    rsx! {
        section { class: "container mx-auto px-4 pb-16 max-w-6xl",
            div { class: "grid lg:grid-cols-2 gap-12 items-center",
                div { class: "space-y-6 lg:order-2",
                    span { class: "block text-xs font-mono uppercase tracking-[0.2em] text-muted",
                        "The name & the mark"
                    }
                    h2 { class: "text-4xl md:text-5xl font-black tracking-tight",
                        "Thermite is rust and aluminium, "
                        span { class: "italic font-serif font-medium", "and it burns through steel." }
                    }
                    p { class: "text-lg text-base-content/70 leading-relaxed",
                        "Hans Goldschmidt mixed the two in Essen in 1895 and found that once lit, the reaction brings its own oxygen: you cannot smother it, water does not put it out, and it runs at 2,500 °C until the fuel is gone. Railways have welded their rails with it ever since. It is what you reach for when the joint has to hold and nothing gentler will do."
                    }
                    p { class: "text-lg text-base-content/70 leading-relaxed",
                        "A production error is the same kind of problem. It does not go out because nobody is looking at it; it keeps burning until someone reads the stack trace and finds the line. Thermite is built so that someone is your coding agent, and the diagnosis is on the issue page before you have opened it. The mark is a molten triangle around a white-hot core — a reaction is hottest at its centre, and so is a stack trace. And yes, it is written in Rust. The pun was free."
                    }
                }

                // Dictionary-style entry
                div { class: "lg:order-1",
                    div { class: "card card-elevated bg-base-200 rounded-3xl p-8 lg:p-10 font-serif",
                        div { class: "flex items-start justify-between gap-6",
                            div {
                                h3 { class: "text-4xl md:text-5xl font-bold", "thermite." }
                                p { class: "mt-3 italic text-base-content/70",
                                    "[thur-mite] "
                                    span { class: "text-muted", "(iron oxide + aluminium)" }
                                    span { class: "not-italic font-bold text-base-content", " noun" }
                                }
                            }
                            div { class: "shrink-0", ThermiteMark { size: 72 } }
                        }
                        div { class: "my-6 border-t border-base-content/20" }
                        p { class: "text-lg leading-relaxed",
                            "A mixture that, once lit, cannot be put out. Also: a small error tracker that hands every crash to your coding agent and "
                            em { "does not let go until it is diagnosed." }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn WhyThermite() -> Element {
    rsx! {
        section { class: "bg-base-200 border-y border-base-300",
            div { class: "container mx-auto px-4 py-16 lg:py-20 max-w-6xl",
                div { class: "max-w-3xl mb-12",
                    span { class: "block text-xs font-mono uppercase tracking-[0.2em] text-muted mb-4",
                        "Why Thermite"
                    }
                    h2 { class: "text-4xl md:text-5xl font-black tracking-tight mb-4",
                        "Built for teams whose agent reads the stack trace."
                    }
                    p { class: "text-base-content/70 text-lg md:text-xl max-w-2xl",
                        "One person, a narrow scope, and a long-term commitment to the people — and the agents — who use it."
                    }
                }

                div { class: "grid md:grid-cols-3 gap-px bg-base-300 border border-base-300 rounded-xl overflow-hidden",
                    WhyCard {
                        persona: "Philosophy",
                        icon: rsx! { Icon { icon: LdBot, width: 22, height: 22 } },
                        title: "Agent first",
                        description: "Every feature starts from one question: does this get a coding agent, or the human reading behind it, from crash to diagnosis faster? If not, it doesn't ship. Thermite never calls a model itself — your agent runs on your machine, under your key.",
                    }
                    WhyCard {
                        persona: "Craft",
                        icon: rsx! { Icon { icon: LdCode, width: 22, height: 22 } },
                        title: "Rust all the way down",
                        description: "Ingest, grouping, the dashboard, the MCP server and the SDK are one Rust workspace. One binary and a Postgres — no Kafka, no ClickHouse, no dozen containers to keep alive at three in the morning.",
                    }
                    WhyCard {
                        persona: "Trust",
                        icon: rsx! { Icon { icon: LdShieldCheck, width: 22, height: 22 } },
                        title: "Built in the EU, hosted where you say",
                        description: "AGPL-3.0: self-host it anywhere and your stack traces never leave a Postgres you control. The hosted version runs on servers in Germany, GDPR by default, with no tracking cookies and no cookie banner to go with them.",
                    }
                }
            }
        }
    }
}

#[component]
fn WhyCard(
    persona: &'static str,
    icon: Element,
    title: &'static str,
    description: &'static str,
) -> Element {
    rsx! {
        div { class: "bg-base-100 p-7 lg:p-8",
            div { class: "flex items-baseline justify-between mb-5",
                span { class: "font-mono text-xs uppercase tracking-[0.2em] text-muted", "{persona}" }
                span { class: "text-primary", {icon} }
            }
            h3 { class: "font-display text-xl font-semibold tracking-tight mb-3", "{title}" }
            p { class: "text-base-content/65 leading-relaxed", "{description}" }
        }
    }
}

/// No form: a mailto is one click and needs no table, no rate limit and no spam filter.
#[component]
fn GetInTouch() -> Element {
    rsx! {
        section { class: "container mx-auto px-4 py-16 max-w-6xl",
            div { class: "rounded-2xl border border-base-300 bg-base-200/60 p-8 text-center",
                span { class: "block text-xs font-mono uppercase tracking-[0.2em] text-muted mb-4",
                    "Get in touch"
                }
                h2 { class: "text-2xl sm:text-3xl font-bold tracking-tight mb-2", "Send me a message" }
                p { class: "text-base-content/60 max-w-xl mx-auto mb-6",
                    "A question, a bug, a Sentry bill you would like to stop paying — write me and I will answer as soon as I can."
                }
                div { class: "flex flex-wrap justify-center gap-3",
                    a {
                        class: "btn btn-primary btn-strong rounded-xl gap-2",
                        href: "mailto:mail@haukejung.de",
                        Icon { icon: LdMail, width: 16, height: 16 }
                        "mail@haukejung.de"
                    }
                    a {
                        class: "btn btn-outline rounded-xl gap-2",
                        href: "https://github.com/hauju/thermite-rs",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        Icon { icon: LdGithub, width: 16, height: 16 }
                        "Source on GitHub"
                    }
                }
            }
        }
    }
}

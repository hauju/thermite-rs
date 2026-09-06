//! The four legal pages. Their content lives as markdown in `assets/legal/` and
//! is rendered to HTML by `build.rs`, so editing a policy is editing prose — no
//! RSX, and no markdown parser in the WASM bundle.

use dioxus::prelude::*;
use dioxus_free_icons::Icon;
use dioxus_free_icons::icons::ld_icons::LdArrowLeft;

use crate::routes::Route;

const IMPRINT_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/imprint.html"));
const PRIVACY_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/privacy.html"));
const TERMS_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/terms.html"));
const COOKIES_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/cookies.html"));

#[component]
pub fn ImprintPage() -> Element {
    rsx! {
        LegalDocument {
            title: "Imprint — Thermite",
            description: "Legal imprint and operator information for Thermite, the self-hosted, Sentry-compatible error tracker.",
            path: "/legal/imprint",
            html: IMPRINT_HTML,
        }
    }
}

#[component]
pub fn PrivacyPage() -> Element {
    rsx! {
        LegalDocument {
            title: "Privacy Policy — Thermite",
            description: "What an error report sent to Thermite contains, what is scrubbed before storage, how long it is kept, and who controls it.",
            path: "/legal/privacy",
            html: PRIVACY_HTML,
        }
    }
}

#[component]
pub fn TermsPage() -> Element {
    rsx! {
        LegalDocument {
            title: "Terms of Service — Thermite",
            description: "The terms and conditions for using Thermite, including DSN keys, quotas, retention, and connecting coding agents over MCP.",
            path: "/legal/terms",
            html: TERMS_HTML,
        }
    }
}

#[component]
pub fn CookiesPage() -> Element {
    rsx! {
        LegalDocument {
            title: "Cookie Policy — Thermite",
            description: "Thermite uses one strictly necessary session cookie and no trackers. What is stored on your device, and why there is no cookie banner.",
            path: "/legal/cookies",
            html: COOKIES_HTML,
        }
    }
}

/// Shared shell: page metadata, a way back, and the prerendered document.
#[component]
fn LegalDocument(
    title: &'static str,
    description: &'static str,
    path: &'static str,
    html: &'static str,
) -> Element {
    rsx! {
        document::Title { "{title}" }
        document::Meta { name: "description", content: description }
        document::Link { rel: "canonical", href: "https://thermite.rs{path}" }

        div { class: "container mx-auto max-w-3xl px-4 pt-10 pb-20",
            Link {
                to: Route::Home {},
                class: "btn btn-ghost btn-sm gap-2 rounded-full mb-8",
                Icon { icon: LdArrowLeft, width: 16, height: 16 }
                "Back to home"
            }
            article {
                class: "prose prose-headings:font-display prose-headings:tracking-tight max-w-none",
                dangerous_inner_html: html,
            }
        }
    }
}

use dioxus::prelude::*;
use dioxus_free_icons::Icon;
use dioxus_free_icons::icons::ld_icons::LdTwitter;

use crate::components::logo::ThermiteMark;
use crate::routes::Route;

/// Site footer for the public layout: a brand column plus three link columns,
/// over a bottom bar carrying the copyright and socials. Rendered by `Navbar`,
/// so every page under the public layout — the landing page and the legal
/// pages — gets it without opting in.
#[component]
pub fn Footer() -> Element {
    rsx! {
        footer { class: "border-t border-base-300 bg-base-200",
            div { class: "mx-auto max-w-7xl px-6 py-12 lg:px-8 lg:py-16",
                div { class: "grid grid-cols-1 gap-10 md:grid-cols-2 lg:grid-cols-4 lg:gap-16",
                    // Brand column
                    div { class: "lg:col-span-1",
                        Link {
                            to: Route::Home {},
                            class: "mb-4 flex items-center gap-2 font-display text-xl font-semibold tracking-tight transition-opacity hover:opacity-80",
                            ThermiteMark { size: 26 }
                            "Thermite"
                        }
                        p { class: "mb-4 text-sm leading-relaxed text-base-content/60",
                            "Self-hosted error tracking that speaks Sentry's wire protocol — and hands every new issue to your coding agent. Built in the 🇪🇺 with Rust 🦀"
                        }
                    }

                    FooterColumn {
                        title: "PRODUCT",
                        links: vec![
                            FooterLink { label: "Pricing".into(), href: FooterHref::Internal(Route::Pricing {}) },
                            FooterLink { label: "Dashboard".into(), href: FooterHref::Internal(Route::Dashboard {}) },
                            FooterLink { label: "Projects".into(), href: FooterHref::Internal(Route::Projects {}) },
                            FooterLink { label: "Sign in".into(), href: FooterHref::Internal(Route::LoginPage { redirect_url: "/dashboard".to_string() }) },
                        ],
                    }

                    FooterColumn {
                        title: "DOCUMENTATION",
                        links: vec![
                            FooterLink { label: "Introduction".into(), href: FooterHref::Internal(Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] }) },
                            FooterLink { label: "Installation".into(), href: FooterHref::Internal(Route::DocsPage { slug: vec!["getting-started".into(), "installation".into()] }) },
                            FooterLink { label: "Authentication".into(), href: FooterHref::Internal(Route::DocsPage { slug: vec!["guides".into(), "authentication".into()] }) },
                            // The unauthenticated discovery page agents read before connecting to /mcp.
                            FooterLink { label: "llms.txt".into(), href: FooterHref::External("/llms.txt".into()) },
                        ],
                    }

                    FooterColumn {
                        title: "LEGAL",
                        links: vec![
                            FooterLink { label: "Imprint".into(), href: FooterHref::Internal(Route::ImprintPage {}) },
                            FooterLink { label: "Privacy Policy".into(), href: FooterHref::Internal(Route::PrivacyPage {}) },
                            FooterLink { label: "Terms of Service".into(), href: FooterHref::Internal(Route::TermsPage {}) },
                            FooterLink { label: "Cookie Policy".into(), href: FooterHref::Internal(Route::CookiesPage {}) },
                        ],
                    }
                }
            }

            // Bottom bar
            div { class: "border-t border-base-300",
                div { class: "mx-auto flex max-w-7xl flex-col items-center justify-between gap-4 px-6 py-6 lg:flex-row lg:px-8",
                    p { class: "text-sm text-base-content/40", "© 2026 Thermite. All rights reserved." }

                    div { class: "flex items-center gap-3",
                        a {
                            href: "https://x.com/haukejung",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            class: "text-base-content/40 transition-colors hover:text-base-content",
                            "aria-label": "Thermite on X",
                            Icon { icon: LdTwitter, width: 20, height: 20 }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum FooterHref {
    Internal(Route),
    External(String),
}

#[derive(Clone, PartialEq)]
struct FooterLink {
    label: String,
    href: FooterHref,
}

#[component]
fn FooterColumn(title: &'static str, links: Vec<FooterLink>) -> Element {
    rsx! {
        div {
            h3 { class: "mb-4 text-xs font-semibold tracking-wider text-base-content/80 uppercase",
                "{title}"
            }
            ul { class: "space-y-3",
                for link in links {
                    li {
                        match &link.href {
                            FooterHref::Internal(route) => rsx! {
                                Link {
                                    to: route.clone(),
                                    class: "text-sm text-base-content/60 transition-colors hover:text-base-content",
                                    "{link.label}"
                                }
                            },
                            FooterHref::External(url) => rsx! {
                                a {
                                    href: "{url}",
                                    class: "text-sm text-base-content/60 transition-colors hover:text-base-content",
                                    "{link.label}"
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::LdGithub};

use crate::UserAuthState;
use crate::components::footer::Footer;
use crate::components::logo::ThermiteMark;
use crate::errors_data::demo_link;
use crate::routes::Route;

/// Public navbar, adapted from mcpi-site: a floating pill detached from the
/// edges — logo left, quiet text links centered, one loud CTA right. The logo
/// goes home, so there is no separate "Home" link. Unlike mcpi there is still no
/// mobile dropdown, so a phone gets the mark, Docs, Pricing and the CTA and
/// nothing else: at 375px the pill has ~326px, and the wordmark, the demo link,
/// the GitHub icon and the quiet "Sign in" beside the CTA all hide below `sm`.
/// Each is reachable elsewhere on a phone (the hero has the demo, the footer
/// has GitHub, the CTA leads to the login). GitHub is an icon rather than a
/// word so it fits beside four labels at `sm`. The demo link exists only where
/// the instance has a demo to point at, and is server-rendered for the same
/// reason the landing page's is: a link appearing after hydration shifts the
/// ones beside it.
#[component]
pub fn Navbar() -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();
    let demo = use_server_future(|| async { demo_link().await.ok().flatten() })?;

    rsx! {
        div { class: "sticky top-0 z-30 px-4 pt-3",
            nav { class: "mx-auto flex max-w-2xl items-center gap-2 rounded-full border border-base-300 bg-base-200/80 py-2 pr-2 pl-4 shadow-lg backdrop-blur",
                Link {
                    to: Route::Home {},
                    class: "inline-flex shrink-0 items-center gap-2 font-display text-lg font-semibold tracking-tight transition-opacity hover:opacity-80",
                    ThermiteMark { size: 22 }
                    span { class: "hidden sm:inline", "Thermite" }
                }

                div { class: "flex-1" }
                div { class: "flex items-center gap-1",
                    Link {
                        to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                        class: "rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content",
                        "Docs"
                    }
                    Link {
                        to: Route::Pricing {},
                        class: "rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content",
                        "Pricing"
                    }
                    if let Some(Some(url)) = demo() {
                        a {
                            href: "{url}",
                            class: "hidden rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content sm:inline-flex",
                            "Demo"
                        }
                    }
                    a {
                        href: "https://github.com/hauju/thermite-rs",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        class: "hidden items-center rounded-full px-3 py-1.5 text-base-content/60 transition-colors hover:text-base-content sm:inline-flex",
                        "aria-label": "Thermite on GitHub",
                        Icon { icon: LdGithub, width: 18, height: 18 }
                    }
                }
                div { class: "flex-1" }

                match &*user_auth.read() {
                    UserAuthState::Authenticated(_) => rsx! {
                        Link {
                            to: Route::Dashboard {},
                            class: "btn btn-primary btn-sm btn-strong shrink-0 rounded-full px-4",
                            "Dashboard"
                        }
                    },
                    // Two doors for a visitor with no session: the loud one is for the reader who
                    // has never been here, the quiet one for the one who already has an account.
                    _ => rsx! {
                        Link {
                            to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                            class: "hidden shrink-0 rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content sm:inline-flex",
                            "Sign in"
                        }
                        Link {
                            to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                            class: "btn btn-primary btn-sm btn-strong shrink-0 rounded-full px-4",
                            "Get Started"
                        }
                    },
                }
            }
        }

        main { class: "min-h-screen bg-base-100",
            Outlet::<Route> {}
        }

        Footer {}
    }
}

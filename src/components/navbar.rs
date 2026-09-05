use dioxus::prelude::*;

use crate::UserAuthState;
use crate::components::footer::Footer;
use crate::components::logo::ThermiteMark;
use crate::routes::Route;

/// Public navbar, adapted from mcpi-site: a floating pill detached from the
/// edges — logo left, quiet text links centered, one loud CTA right. The logo
/// goes home, so there is no separate "Home" link. Unlike mcpi there is no
/// mobile dropdown: two quiet links still fit the pill at every width.
#[component]
pub fn Navbar() -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();

    rsx! {
        div { class: "sticky top-0 z-30 px-4 pt-3",
            nav { class: "mx-auto flex max-w-2xl items-center gap-2 rounded-full border border-base-300 bg-base-200/80 py-2 pr-2 pl-4 shadow-lg backdrop-blur",
                Link {
                    to: Route::Home {},
                    class: "inline-flex shrink-0 items-center gap-2 font-display text-lg font-semibold tracking-tight transition-opacity hover:opacity-80",
                    ThermiteMark { size: 22 }
                    "Thermite"
                }

                div { class: "flex-1" }
                div { class: "flex items-center gap-1",
                    Link {
                        to: Route::Pricing {},
                        class: "rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content",
                        "Pricing"
                    }
                    Link {
                        to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                        class: "rounded-full px-3 py-1.5 text-sm font-medium text-base-content/60 transition-colors hover:text-base-content",
                        "Docs"
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
                    _ => rsx! {
                        Link {
                            to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                            class: "btn btn-primary btn-sm btn-strong shrink-0 rounded-full px-4",
                            "Sign In"
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

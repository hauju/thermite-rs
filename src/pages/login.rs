use dioxus::prelude::*;

use crate::UserAuthState;
use crate::components::logo::ThermiteMark;
use crate::errors_data::{demo_autologin, local_login, site_enabled};
use crate::routes::Route;
use crate::waitlist::waitlist_open;

/// Login page that wraps the auth crate's LoginPage component.
#[component]
pub fn LoginPage(redirect_url: String) -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();
    let nav = use_navigator();

    // If already authenticated, redirect to dashboard
    use_effect(move || {
        if matches!(&*user_auth.read(), UserAuthState::Authenticated(_)) {
            nav.push(Route::Dashboard {});
        }
    });

    // A public sandbox has no login form: it signs the visitor in itself.
    let autologin = use_resource(|| async { demo_autologin().await.unwrap_or(false) });
    // While hosted signup is behind the waitlist, the reassurance line must not promise a
    // free start that the registration gate will refuse.
    let waitlist = use_resource(|| async { waitlist_open().await.unwrap_or(false) });
    // Which login flow this instance runs is a server fact, so it is fetched rather than
    // guessed. Both sides start at `None` and render the same skeleton, so hydration matches.
    let local = use_resource(|| async { local_login().await.unwrap_or(false) });
    // Whether there is a landing page behind the "Back" link at all.
    let site = use_resource(|| async { site_enabled().await.unwrap_or(false) });
    use_effect(move || {
        if autologin() == Some(true) {
            let _ = document::eval("window.location.href = '/demo';");
        }
    });

    rsx! {
        div { class: "relative min-h-screen overflow-hidden flex items-center justify-center p-4",
            // Background layer — gradient mesh
            div { class: "absolute inset-0 bg-gradient-to-br from-base-100 via-base-100 to-primary/5" }
            div { class: "absolute top-20 right-20 w-[28rem] h-[28rem] bg-primary/8 rounded-full blur-[100px]" }
            div { class: "absolute bottom-40 left-10 w-80 h-80 bg-secondary/8 rounded-full blur-[80px]" }
            div { class: "absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[40rem] h-[40rem] bg-primary/5 rounded-full blur-[120px]" }

            // Back to home (subtle, floating). Only where there is a landing page to go back
            // to: without the marketing site, `/` is a redirect straight back here.
            if site() == Some(true) {
                div { class: "absolute top-4 left-4 z-20",
                    Link {
                        to: Route::Home {},
                        class: "btn btn-ghost btn-sm gap-2",
                        svg {
                            xmlns: "http://www.w3.org/2000/svg",
                            class: "h-4 w-4",
                            fill: "none",
                            view_box: "0 0 24 24",
                            stroke: "currentColor",
                            path {
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                stroke_width: "2",
                                d: "M15 19l-7-7 7-7",
                            }
                        }
                        "Back"
                    }
                }
            }

            // Local sign-in: dx-auth's page owns the whole thing — its own card, heading and
            // hydration gate, with no `embed` prop to strip them — so it replaces the branded
            // card rather than nesting inside one.
            if local() == Some(true) {
                div { class: "relative z-10 w-full",
                    auth::LocalLoginPage {
                        redirect_url: redirect_url.clone(),
                        app_name: "Thermite".to_string(),
                        // Same reasoning as the FerrisKey page below: a full page load, not a
                        // router push, so /oauth/authorize/resume is reached as an Axum route.
                        on_success: move |url: String| {
                            nav.push(NavigationTarget::<Route>::External(url));
                        },
                    }
                }
            }

            // Login card
            if local() != Some(true) {
                div { class: "relative z-10 w-full max-w-md",
                div { class: "relative rounded-2xl bg-base-200/60 backdrop-blur-xl border border-base-300/50 shadow-2xl overflow-hidden",
                    // Top accent line
                    div { class: "absolute top-0 left-8 right-8 h-px bg-gradient-to-r from-transparent via-primary/50 to-transparent" }

                    div { class: "p-8",
                        // Logo / brand
                        div { class: "text-center mb-6",
                            div { class: "inline-flex items-center justify-center w-16 h-16 rounded-2xl bg-base-200 border border-base-300 shadow-lg shadow-primary/10 mb-4",
                                ThermiteMark { size: 34 }
                            }
                            h1 { class: "text-2xl font-semibold tracking-tight", "Thermite" }
                            p { class: "text-sm text-base-content/60 mt-1", "Sign in or create an account" }
                        }

                        // Auth crate's LoginPage (embedded — no wrapper/header)
                        if local() == Some(false) {
                            auth::LoginPage {
                                redirect_url: redirect_url.clone(),
                                // The crate hands back the validated destination; navigating is the
                                // host app's job. A full page load, not a router push: the MCP connect
                                // flow resumes at /oauth/authorize/resume, an Axum route the router
                                // would treat as a 404 — and a handler that dropped the URL stranded
                                // every connect on the dashboard instead of the consent page.
                                on_success: move |url: String| {
                                    nav.push(NavigationTarget::<Route>::External(url));
                                },
                                embed: true,
                            }
                        } else {
                            // Still asking which flow to render — the card keeps its shape so the
                            // page does not jump once the answer lands.
                            div { class: "flex justify-center py-10",
                                span { class: "loading loading-spinner loading-md text-primary" }
                            }
                        }

                        // Reassurance for new signups — and only for them. While signup is
                        // closed this said the opposite of the crate's own line two rows above
                        // it ("No account yet? We'll create one for you."), so the page made a
                        // promise and withdrew it in the same breath. With the marketing CTA
                        // now pointing at the waitlist instead of here, whoever reaches this
                        // page came to sign in, and the page says one thing.
                        if waitlist() == Some(false) {
                            p { class: "mt-6 text-center text-xs text-base-content/50",
                                "New here? Free to start \u{00b7} no credit card required."
                            }
                        }
                    }
                }
                }
            }
        }
    }
}

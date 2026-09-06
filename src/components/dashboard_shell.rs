use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

use crate::UserAuthState;
use crate::components::command_palette::CommandPalette;
use crate::components::logo::ThermiteMark;
use crate::components::theme_toggle::ThemeToggle;
use crate::errors_data::demo_project;
use crate::routes::Route;
use crate::version::{UpdateBanner, use_build_check, use_reload_when_stale};

/// Whether a route is one a visitor may see without signing in: the demo project's board, or
/// an issue page — whether that issue belongs to the demo project is the server's call, and a
/// wrong guess gets an error page rather than data.
fn visitor_route(route: &Route, demo: Option<&str>) -> bool {
    match (route, demo) {
        (Route::Issues { slug, .. }, Some(demo)) => slug == demo,
        (Route::IssueDetail { .. }, Some(_)) => true,
        _ => false,
    }
}

/// Auth-gated dashboard layout with sidebar navigation. Redirects to login when the user is not
/// authenticated — unless a demo project is configured and this is a route into it, which a
/// visitor gets read-only, under a banner.
#[component]
pub fn DashboardShell() -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();
    let nav = use_navigator();
    let mut palette = use_signal(|| false);
    let route = use_route::<Route>();
    let demo = use_resource(|| async { demo_project().await.ok().flatten() });
    use_build_check();
    use_reload_when_stale();

    // Redirect to login when not authenticated. Waits until the demo slug is known, or every
    // visitor would be bounced before the shell could tell them apart.
    use_effect(move || {
        let anonymous = matches!(&*user_auth.read(), UserAuthState::NotAuthenticated);
        let demo = demo.read();
        let Some(demo) = &*demo else { return };
        let route = router().current::<Route>();
        if anonymous && !visitor_route(&route, demo.as_deref()) {
            nav.push(Route::LoginPage {
                redirect_url: "/dashboard".to_string(),
            });
        }
    });

    let visiting = match &*demo.read() {
        Some(demo) => visitor_route(&route, demo.as_deref()),
        None => false,
    };

    match &*user_auth.read() {
        UserAuthState::Loading => rsx! {
            div { class: "flex items-center justify-center min-h-screen",
                span { class: "loading loading-spinner loading-lg text-primary" }
            }
        },
        UserAuthState::NotAuthenticated if visiting => rsx! { VisitorShell {} },
        UserAuthState::NotAuthenticated => rsx! {},
        UserAuthState::Authenticated(user) => {
            let username = user.username.clone();
            let email = user.email.clone();
            let avatar_url = user.avatar_url.clone();
            rsx! {
                div { class: "drawer lg:drawer-open admin-shell",
                    input {
                        id: "dashboard-drawer",
                        r#type: "checkbox",
                        class: "drawer-toggle",
                    }

                    // Main content
                    div { class: "drawer-content flex flex-col",
                        UpdateBanner {}
                        // Top bar (mobile drawer toggle)
                        div { class: "navbar glass-panel border-b border-base-300 lg:hidden",
                            div { class: "flex-none",
                                label {
                                    r#for: "dashboard-drawer",
                                    class: "btn btn-square btn-ghost drawer-button",
                                    svg {
                                        xmlns: "http://www.w3.org/2000/svg",
                                        fill: "none",
                                        view_box: "0 0 24 24",
                                        class: "inline-block w-5 h-5 stroke-current",
                                        path {
                                            stroke_linecap: "round",
                                            stroke_linejoin: "round",
                                            stroke_width: "2",
                                            d: "M4 6h16M4 12h16M4 18h16",
                                        }
                                    }
                                }
                            }
                            div { class: "flex-1",
                                span { class: "inline-flex items-center gap-2 font-display text-lg font-semibold tracking-tight",
                                    ThermiteMark { size: 22 }
                                    "Thermite"
                                }
                            }
                        }

                        // Page content
                        div { class: "flex-1 p-4 lg:p-8 animate-fade-up",
                            Outlet::<Route> {}
                        }
                        CommandPalette { open: palette }
                    }

                    // Sidebar
                    div { class: "drawer-side z-40",
                        label {
                            r#for: "dashboard-drawer",
                            class: "drawer-overlay",
                        }
                        aside { class: "border-r border-base-300 w-64 min-h-full flex flex-col",
                            // Logo / brand
                            div { class: "p-4 border-b border-base-300",
                                Link {
                                    to: Route::Home {},
                                    class: "inline-flex items-center gap-2 font-display text-xl font-semibold tracking-tight",
                                    ThermiteMark { size: 26 }
                                    "Thermite"
                                }
                            }

                            // Navigation
                            nav { class: "flex-1 flex flex-col gap-1 p-4",
                                button {
                                    class: "nav-link flex items-center text-sm font-medium text-base-content/70 w-full",
                                    onclick: move |_| palette.set(true),
                                    Icon { icon: LdSearch, width: 18, height: 18 }
                                    "Jump to…"
                                    kbd { class: "kbd kbd-xs ml-auto", "⌘K" }
                                }
                                NavItem {
                                    to: Route::Dashboard {},
                                    icon: rsx! { Icon { icon: LdLayoutDashboard, width: 18, height: 18 } },
                                    label: "Dashboard",
                                }
                                NavItem {
                                    to: Route::Projects {},
                                    // Deliberately not a warning triangle: that is the brand mark's
                                    // silhouette, and it renders 40px above this in the same sidebar.
                                    icon: rsx! { Icon { icon: LdLayers, width: 18, height: 18 } },
                                    label: "Projects",
                                }
                                NavItem {
                                    to: Route::Playground {},
                                    icon: rsx! { Icon { icon: LdFlaskConical, width: 18, height: 18 } },
                                    label: "Playground",
                                }
                                NavItem {
                                    to: Route::Settings {},
                                    icon: rsx! { Icon { icon: LdSettings, width: 18, height: 18 } },
                                    label: "Settings",
                                }
                            }

                            // User menu at bottom
                            div { class: "p-3 border-t border-base-300",
                                UserMenu { username, email, avatar_url }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The shell a visitor gets on the demo project: the page under a banner that says so, and a
/// sidebar that only offers the docs and the way in. No project list, no settings, no palette —
/// none of it would answer for a visitor anyway.
#[component]
fn VisitorShell() -> Element {
    rsx! {
        div { class: "drawer lg:drawer-open admin-shell",
            input {
                id: "dashboard-drawer",
                r#type: "checkbox",
                class: "drawer-toggle",
            }
            div { class: "drawer-content flex flex-col",
                UpdateBanner {}
                div { class: "flex items-center gap-3 px-4 py-2 text-sm border-b border-warning/40 bg-warning/10",
                    label {
                        r#for: "dashboard-drawer",
                        class: "btn btn-xs btn-ghost lg:hidden",
                        "Menu"
                    }
                    Icon { icon: LdEye, width: 16, height: 16, class: "text-warning shrink-0" }
                    span { "You are looking at the live demo, read-only. Sign in to get a project of your own." }
                    Link {
                        to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                        class: "btn btn-xs btn-primary ml-auto shrink-0",
                        "Sign in"
                    }
                }
                div { class: "flex-1 p-4 lg:p-8 animate-fade-up",
                    Outlet::<Route> {}
                }
            }
            div { class: "drawer-side z-40",
                label {
                    r#for: "dashboard-drawer",
                    class: "drawer-overlay",
                }
                aside { class: "border-r border-base-300 w-64 min-h-full flex flex-col",
                    div { class: "p-4 border-b border-base-300",
                        Link {
                            to: Route::Home {},
                            class: "inline-flex items-center gap-2 font-display text-xl font-semibold tracking-tight",
                            ThermiteMark { size: 26 }
                            "Thermite"
                        }
                    }
                    nav { class: "flex-1 flex flex-col gap-1 p-4",
                        NavItem {
                            to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                            icon: rsx! { Icon { icon: LdBookOpen, width: 18, height: 18 } },
                            label: "Docs",
                        }
                        NavItem {
                            to: Route::LoginPage { redirect_url: "/dashboard".to_string() },
                            icon: rsx! { Icon { icon: LdLogIn, width: 18, height: 18 } },
                            label: "Sign in",
                        }
                    }
                }
            }
        }
    }
}

/// Sidebar navigation link with icon + active-route highlighting.
#[component]
fn NavItem(to: Route, icon: Element, label: &'static str) -> Element {
    rsx! {
        Link {
            to,
            class: "nav-link flex items-center text-sm font-medium text-base-content/70",
            active_class: "active",
            {icon}
            "{label}"
        }
    }
}

/// User dropdown at the foot of the sidebar: avatar + identity trigger opening a
/// menu with Settings and Logout.
#[component]
fn UserMenu(username: String, email: String, avatar_url: Option<String>) -> Element {
    let initial = username
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();

    rsx! {
        div { class: "dropdown dropdown-top w-full",
            // Trigger
            div {
                tabindex: 0,
                role: "button",
                class: "btn btn-ghost w-full justify-start gap-3 px-2 h-auto py-2 rounded-xl",
                if let Some(url) = avatar_url.clone() {
                    img {
                        src: "{url}",
                        alt: "Avatar",
                        class: "w-8 h-8 rounded-full shrink-0 object-cover",
                    }
                } else {
                    div { class: "w-8 h-8 rounded-full bg-primary/15 text-primary flex items-center justify-center text-xs font-semibold shrink-0",
                        "{initial}"
                    }
                }
                div { class: "min-w-0 flex-1 text-left",
                    div { class: "text-sm font-medium truncate", "{username}" }
                    div { class: "text-xs text-base-content/50 truncate", "{email}" }
                }
                Icon {
                    icon: LdChevronUp,
                    width: 16,
                    height: 16,
                    class: "shrink-0 opacity-50",
                }
            }
            // Menu
            ul {
                tabindex: 0,
                class: "dropdown-content menu bg-base-100 rounded-box w-full p-2 shadow-lg border border-base-300 mb-2",
                li {
                    Link {
                        to: Route::Settings {},
                        Icon { icon: LdSettings, width: 16, height: 16 }
                        "Settings"
                    }
                }
                li {
                    ThemeToggle {}
                }
                div { class: "divider my-0" }
                li {
                    button {
                        onclick: move |_| {
                            // Full-page POST + reload: flushes the session server-side
                            // and drops all client auth state. CSRF passes (same origin).
                            let _ = document::eval(
                                "fetch('/auth/logout', { method: 'POST' })\
                                 .finally(function () { window.location.href = '/'; });",
                            );
                        },
                        Icon { icon: LdLogOut, width: 16, height: 16 }
                        "Logout"
                    }
                }
            }
        }
    }
}

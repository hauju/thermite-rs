//! Noticing a deploy from a tab that was open before it.
//!
//! Client-side routing never refetches the app, so a tab opened in the morning keeps the
//! morning's client through every rollout — until a server function rejects an argument shape
//! that no longer exists and the page shows raw JSON. Both binaries carry the commit they were
//! built from (`build.rs`); the client compares it to the server's whenever the tab comes back
//! into view, and a mismatch turns the next page change into a full load.

use std::fmt::Display;

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::LdRefreshCw};

use crate::routes::Route;

/// The commit this binary was built from. Server and client come out of one tree, so they agree
/// unless a deploy happened in between.
pub const BUILD: &str = env!("THERMITE_BUILD");

/// The build the server is running, for a client to compare against its own.
#[get("/api/version")]
pub async fn build_id() -> Result<String, ServerFnError> {
    Ok(BUILD.to_string())
}

/// The server's build, once it has been seen to differ from this client's.
static NEWER_BUILD: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Whether the server has moved on from this client.
pub fn stale() -> bool {
    NEWER_BUILD.read().is_some()
}

/// The message for a failed load: the error itself, unless the tab is known to be out of date,
/// in which case the error is a symptom and reloading is the fix.
pub fn load_error(what: &str, error: impl Display) -> String {
    if stale() {
        format!("{what}: this tab is running an older version of Thermite. Reload to continue.")
    } else {
        format!("{what}: {error}")
    }
}

#[cfg(not(feature = "server"))]
async fn check() {
    let Ok(server) = build_id().await else { return };
    if server != BUILD && NEWER_BUILD.peek().is_none() {
        *NEWER_BUILD.write() = Some(server);
    }
}

/// Compares builds whenever the tab comes back into view, and every few minutes while it stays
/// there. A stale tab is nearly always one you return to, so the visibility check is the one that
/// matters; the interval covers a tab that never left the foreground.
pub fn use_build_check() {
    #[cfg(not(feature = "server"))]
    {
        use crate::components::toast::sleep;

        use_future(|| async {
            let mut visible = document::eval(
                "document.addEventListener('visibilitychange', () => {\
                    if (document.visibilityState === 'visible') dioxus.send(true);\
                });",
            );
            while visible.recv::<bool>().await.is_ok() {
                check().await;
            }
        });
        use_future(|| async {
            loop {
                sleep(5 * 60 * 1000).await;
                check().await;
            }
        });
    }
}

/// Turns the first page change after a deploy into a full load, which picks up the new client
/// and keeps the URL — and with it the filters, which live there. A change to the query string
/// alone does not count: a reload mid-search would eat the keystrokes.
pub fn use_reload_when_stale() {
    let page = use_memo(|| page_of(&router().current::<Route>()));
    use_effect(move || {
        let _ = page();
        // Peeked, not read: a build turning stale must not reload the page someone is on.
        if let Some(build) = NEWER_BUILD.peek().clone() {
            reload_once(&build);
        }
    });
}

/// The route without its query string.
fn page_of(route: &Route) -> String {
    let route = route.to_string();
    route.split('?').next().unwrap_or(&route).to_string()
}

/// Reloads for the given build once per tab, so a client that still disagrees afterwards — a
/// cached bundle, a rollout still in progress — gets the banner and not a loop.
fn reload_once(build: &str) {
    let eval = document::eval(
        "const build = await dioxus.recv();\
         const key = 'thermite:reloaded-for';\
         if (sessionStorage.getItem(key) !== build) {\
             sessionStorage.setItem(key, build);\
             location.reload();\
         }",
    );
    let _ = eval.send(build.to_string());
}

/// The one-line notice that a newer build is out. Page changes reload on their own; this is for
/// a tab that stays where it is.
#[component]
pub fn UpdateBanner() -> Element {
    if !stale() {
        return rsx! {};
    }
    rsx! {
        div { class: "flex items-center gap-3 px-4 py-2 text-sm border-b border-info/40 bg-info/10",
            Icon { icon: LdRefreshCw, width: 16, height: 16, class: "text-info shrink-0" }
            span { "Thermite was updated since this tab was opened." }
            button {
                class: "btn btn-xs btn-primary ml-auto shrink-0",
                onclick: move |_| {
                    let _ = document::eval("location.reload();");
                },
                "Reload"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::page_of;
    use crate::routes::Route;

    #[test]
    fn page_ignores_the_query_string() {
        let with: Route = "/projects/demo?status=resolved&q=timeout".parse().unwrap();
        let without: Route = "/projects/demo".parse().unwrap();
        assert_eq!(page_of(&with), "/projects/demo");
        assert_eq!(page_of(&with), page_of(&without));
        assert_ne!(page_of(&with), page_of(&"/projects/other".parse().unwrap()));
    }
}

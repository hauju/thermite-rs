use dioxus::prelude::*;
use dioxus_docs_kit::{
    DocsConfig, DocsContext, DocsLayout, DocsPageContent, DocsRegistry, SearchButton,
    use_docs_providers,
};
use dioxus_free_icons::Icon;
use dioxus_free_icons::icons::ld_icons::LdMenu;
use std::sync::LazyLock;

use crate::components::logo::ThermiteMark;
use crate::components::meta::{PageMeta, SITE_DESCRIPTION};
use crate::errors_data::site_enabled;
use crate::routes::Route;

// ============================================================================
// Documentation Registry
// ============================================================================

dioxus_docs_kit::doc_content_map!();

pub static DOCS: LazyLock<DocsRegistry> = LazyLock::new(|| {
    DocsConfig::new(include_str!("../../docs/_nav.json"), doc_content_map())
        .with_default_path("getting-started/introduction")
        .build()
});

// ============================================================================
// Docs Layout
// ============================================================================

/// Layout wrapper that wires DocsContext + DocsRegistry into DocsLayout.
#[component]
pub fn DocsShell() -> Element {
    let nav = use_navigator();
    let route = use_route::<Route>();
    // The docs are served on every instance; the landing page is not. Both sides start at
    // `None`, so the header hydrates unchanged.
    let site = use_resource(|| async { site_enabled().await.unwrap_or(false) });

    let current_path = use_memo(move || match route.clone() {
        Route::DocsPage { slug } => slug.join("/"),
        _ => String::new(),
    });

    let mut docs_ctx = DocsContext::new(
        current_path,
        "/docs",
        Callback::new(move |path: String| {
            let slug: Vec<String> = path.split('/').map(String::from).collect();
            nav.push(Route::DocsPage { slug });
        }),
    );
    // The kit's own head tags are off: it writes a second `og:type` and a `twitter:card` of
    // `summary` beside the root's, and cannot emit a canonical without an origin the client
    // does not have. `DocsPage` writes the page tags from the same registry instead; what that
    // forgoes is the kit's `TechArticle` JSON-LD.
    docs_ctx.auto_meta = false;

    let providers = use_docs_providers(&DOCS, docs_ctx);
    let search_open = providers.search_open;
    let mut drawer_open = providers.drawer_open;
    // Where the mark goes: the landing page where there is one, the application otherwise.
    let brand = if site() == Some(true) {
        Route::Home {}
    } else {
        Route::Dashboard {}
    };

    rsx! {
        DocsLayout {
            header: rsx! {
                div { class: "navbar bg-base-200 border-b border-base-300 px-4 lg:px-8",
                    div { class: "flex-1 gap-2",
                        button {
                            class: "btn btn-ghost btn-sm btn-square lg:hidden docs-menu-btn",
                            onclick: move |_| drawer_open.toggle(),
                            Icon { class: "size-5", icon: LdMenu }
                        }
                        Link {
                            to: brand,
                            class: "inline-flex items-center gap-2 font-display text-xl font-semibold tracking-tight hover:opacity-80 transition-opacity",
                            ThermiteMark { size: 24 }
                            "Thermite"
                        }
                    }
                    div { class: "flex-none flex items-center gap-1",
                        ul { class: "menu menu-horizontal gap-1 hidden lg:flex",
                            if site() == Some(true) {
                                li {
                                    Link {
                                        to: Route::Home {},
                                        class: "btn btn-ghost btn-sm rounded-lg font-medium",
                                        "Home"
                                    }
                                }
                            }
                            li {
                                Link {
                                    to: Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] },
                                    class: "btn btn-ghost btn-sm rounded-lg font-medium",
                                    "Docs"
                                }
                            }
                        }
                        SearchButton { search_open }
                    }
                }
            },
            Outlet::<Route> {}
        }
    }
}

// ============================================================================
// Docs Page
// ============================================================================

/// Renders a documentation page based on the URL slug.
#[component]
pub fn DocsPage(slug: Vec<String>) -> Element {
    let path = slug.join("/");
    // Both sides hold the registry, so server and client agree on whether the tags exist.
    let title = DOCS.get_page_title(&path);
    rsx! {
        if let Some(title) = title {
            PageMeta {
                title: format!("{title} — Thermite"),
                // A page without a frontmatter description gets the site's rather than a blank
                // subtitle under its card.
                description: DOCS
                    .get_page_description(&path)
                    .unwrap_or_else(|| SITE_DESCRIPTION.to_string()),
                path: format!("/docs/{path}"),
            }
            // The same page as Markdown (served by server::seo), for agents and "view as
            // Markdown" tooling. Every page here has a source: there are no OpenAPI pages.
            document::Link { rel: "alternate", r#type: "text/markdown", href: "/docs/{path}.md" }
        }
        DocsPageContent { path }
    }
}

//! What a search engine is told: `robots.txt`, `sitemap.xml`, which pages are `noindex`, and
//! the two patches the SSR shell needs on the way out — the `lang` attribute Dioxus leaves off
//! `<html>`, and the analytics tag, which is configured at runtime and so cannot come from the
//! client bundle.
//!
//! The split follows Google's own guidance: `robots.txt` only keeps crawlers off the machine
//! endpoints, while the pages that must stay out of the index say so with `X-Robots-Tag`. A
//! `Disallow` would do the opposite of what is wanted twice over — a blocked URL can still be
//! indexed from links alone, and link-preview crawlers honour `robots.txt` too, so an issue link
//! pasted into a chat would lose its card. The whole demo instance is `noindex` for the same
//! reason: it duplicates every marketing page, but its links must still unfurl.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::components::meta::SITE_DESCRIPTION;
use crate::pages::docs::DOCS;

const TEXT: &str = "text/plain; charset=utf-8";
const XML: &str = "application/xml; charset=utf-8";
const MARKDOWN: &str = "text/markdown; charset=utf-8";

/// The marketing and legal pages. Docs pages come from the docs registry, so a new page is in
/// the sitemap the moment it is in the navigation.
const PUBLIC_PAGES: &[&str] = &[
    "/",
    "/about",
    "/pricing",
    "/legal/imprint",
    "/legal/privacy",
    "/legal/terms",
    "/legal/cookies",
];

/// The application behind the login and the routes that sign a visitor in. What a crawler
/// gets here is the empty shell or a redirect, neither of which belongs in a search result.
const NOINDEX_PREFIXES: &[&str] = &[
    "/dashboard",
    "/projects",
    "/issues",
    "/settings",
    "/playground",
    "/login",
    "/demo",
    // The consent page: HTML that a Disallow alone would still let into the index.
    "/oauth",
];

pub fn is_noindex_path(path: &str) -> bool {
    NOINDEX_PREFIXES.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// `X-Robots-Tag: noindex`, for the pages that must stay out of the index while remaining
/// fetchable: the application pages behind the login, and every page of the demo instance.
pub fn mark_noindex(headers: &mut HeaderMap) {
    headers.insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex"),
    );
}

/// `robots.txt`: keep crawlers off the endpoints that only SDKs, agents and probes talk to.
/// Nothing a person shares is listed. The demo instance advertises no sitemap, since every
/// page there is `noindex`.
pub fn robots_txt(origin: &str, demo: bool) -> String {
    let mut out = String::from(
        "User-agent: *\n\
         Disallow: /api/\n\
         Disallow: /mcp\n\
         Disallow: /oauth/\n\
         Disallow: /health\n\
         Disallow: /ready\n",
    );
    if !demo {
        out.push_str(&format!("\nSitemap: {origin}/sitemap.xml\n"));
    }
    out
}

/// `sitemap.xml`: the public pages and every docs page. No `lastmod`, `changefreq` or
/// `priority` — nothing here knows when a page changed, and the crawlers ignore the other two.
///
/// Without the marketing site (`THERMITE_SITE` unset) the docs are all there is: the other
/// entries would point at the 404 the router answers there.
pub fn sitemap_xml(origin: &str, site: bool) -> String {
    let mut docs: Vec<String> = DOCS
        .get_all_paths()
        .into_iter()
        .map(|path| format!("/docs/{path}"))
        .collect();
    docs.sort_unstable();

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    let public = if site { PUBLIC_PAGES } else { &[] };
    for path in public
        .iter()
        .copied()
        .chain(docs.iter().map(String::as_str))
    {
        out.push_str(&format!("  <url><loc>{origin}{path}</loc></url>\n"));
    }
    out.push_str("</urlset>\n");
    out
}

/// `/robots.txt`, the docs as Markdown, and `/sitemap.xml` on every instance but the demo: a
/// sitemap of `noindex` URLs is a contradiction the crawlers that probe the path directly would
/// still read.
///
/// The Markdown routes — every docs page at `/docs/<path>.md` and the whole set at
/// `/llms-full.txt` — are what an agent reads instead of the rendered page, and `/llms.txt`
/// points at them. They come from the docs registry the way the kit's own `SeoRouter` builds
/// them; that router is not mounted because it also claims `/llms.txt`, `/robots.txt` and
/// `/sitemap.xml`, each of which says more on this instance than the docs alone.
pub fn seo_router(origin: &str, demo: bool, site: bool) -> Router {
    let origin = origin.trim_end_matches('/');
    let robots = robots_txt(origin, demo);
    let mut router = Router::new().route(
        "/robots.txt",
        get(move || async move { text(TEXT, robots) }),
    );
    for path in DOCS.get_all_paths() {
        if let Some(markdown) = DOCS.get_doc_content(path) {
            router = router.route(
                &format!("/docs/{path}.md"),
                get(move || async move { text(MARKDOWN, markdown) }),
            );
        }
    }
    let full = DOCS.generate_llms_full_txt("Thermite", SITE_DESCRIPTION, &format!("{origin}/docs"));
    router = router.route(
        "/llms-full.txt",
        get(move || async move { text(TEXT, full) }),
    );
    if demo {
        return router;
    }
    let sitemap = sitemap_xml(origin, site);
    router.route(
        "/sitemap.xml",
        get(move || async move { text(XML, sitemap) }),
    )
}

fn text(content_type: &'static str, body: impl IntoResponse) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        body,
    )
        .into_response()
}

/// Patches the SSR shell on the way out, on `text/html` responses only: `lang="en"` onto the
/// bare `<html>` tag Dioxus emits, and — when a website id is configured — the Umami tracker
/// tag before `</head>`. Patching the page here beats carrying a copy of `dx`'s index template
/// that drifts with every CLI release, and it is the only place the tracker tag can come from:
/// the client bundle is built long before an operator sets `UMAMI_WEBSITE_ID`, and a head
/// component that renders conditionally would shift the hydration entries under it.
///
/// Two things this leans on. It must sit inside the compression layer, where the body is still
/// readable. And it buffers the whole body, which costs nothing while SSR streaming stays off
/// (`StreamingMode::Disabled`, the default `dioxus::server::router` uses): the page is complete
/// before its first byte is sent anyway. Enabling out-of-order streaming would need this to
/// patch the first chunk and pass the rest through instead.
pub async fn patch_shell(
    State(umami_website_id): State<Option<String>>,
    request: Request,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"))
        && !response.headers().contains_key(header::CONTENT_ENCODING);
    if !is_html {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        // A render that failed mid-body must not become a well-formed, cacheable empty page.
        Err(_) => {
            parts.status = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
            parts.headers.remove(header::CONTENT_LENGTH);
            return Response::from_parts(parts, Body::empty());
        }
    };
    let Ok(html) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let mut patched = add_lang(html);
    if let Some(id) = umami_website_id.as_deref()
        && let Some(tagged) = add_umami_tag(patched.as_deref().unwrap_or(html), id)
    {
        patched = Some(tagged);
    }
    let Some(patched) = patched else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    // The length changed; hyper derives a fresh one from the new body.
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(patched))
}

/// The document with `lang="en"` on its `<html>` tag, or `None` when there is nothing to do.
fn add_lang(html: &str) -> Option<String> {
    let start = html.find("<html")?;
    let end = start + html[start..].find('>')?;
    if html[start..end].contains(" lang=") {
        return None;
    }
    Some(html.replacen("<html", "<html lang=\"en\"", 1))
}

/// The document with the Umami tracker tag last in `<head>`, or `None` when there is no head
/// to put it in. The id is validated where it is read (`config::umami_website_id`), so nothing
/// here has to escape it.
fn add_umami_tag(html: &str, website_id: &str) -> Option<String> {
    let at = html.find("</head>")?;
    let tag = format!(r#"<script defer src="/stats.js" data-website-id="{website_id}"></script>"#);
    let mut out = String::with_capacity(html.len() + tag.len());
    out.push_str(&html[..at]);
    out.push_str(&tag);
    out.push_str(&html[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_application_pages_are_noindex() {
        for path in [
            "/dashboard",
            "/projects/demo",
            "/issues/7",
            "/login",
            "/demo",
            "/oauth/authorize",
        ] {
            assert!(is_noindex_path(path), "{path}");
        }
        for path in [
            "/",
            "/pricing",
            "/docs/getting-started/introduction",
            "/projectsx",
        ] {
            assert!(!is_noindex_path(path), "{path}");
        }
    }

    #[test]
    fn lang_is_added_once_and_only_where_missing() {
        assert_eq!(
            add_lang("<!DOCTYPE html><html><head></head></html>").as_deref(),
            Some("<!DOCTYPE html><html lang=\"en\"><head></head></html>")
        );
        assert_eq!(add_lang("<html lang=\"de\"><head></head></html>"), None);
        assert_eq!(add_lang("no document here"), None);
    }

    #[test]
    fn the_tracker_tag_is_the_last_thing_in_the_head() {
        let id = "af413d12-39d7-4060-bc8d-6856f5b74ae1";
        assert_eq!(
            add_umami_tag(
                "<html><head><title>t</title></head><body></body></html>",
                id
            )
            .as_deref(),
            Some(concat!(
                "<html><head><title>t</title>",
                r#"<script defer src="/stats.js" data-website-id="af413d12-39d7-4060-bc8d-6856f5b74ae1"></script>"#,
                "</head><body></body></html>"
            ))
        );
        assert_eq!(add_umami_tag("no document here", id), None);
    }

    /// Without the marketing site those URLs are a 404, and a sitemap that lists them is a
    /// crawl budget spent on nothing.
    #[test]
    fn the_sitemap_lists_the_marketing_pages_only_where_they_exist() {
        let with_site = sitemap_xml("https://thermite.rs", true);
        assert!(with_site.contains("<loc>https://thermite.rs/pricing</loc>"));

        let docs_only = sitemap_xml("https://errors.example.com", false);
        assert!(!docs_only.contains("/about"), "{docs_only}");
        assert!(!docs_only.contains("/pricing"), "{docs_only}");
        assert!(!docs_only.contains("/legal/"), "{docs_only}");
        assert!(
            docs_only.contains("<loc>https://errors.example.com/docs/"),
            "the docs stay: a self-hoster needs the SDK, MCP and cron pages\n{docs_only}"
        );
    }

    #[test]
    fn the_demo_instance_advertises_no_sitemap() {
        assert!(
            robots_txt("https://thermite.rs", false)
                .contains("Sitemap: https://thermite.rs/sitemap.xml")
        );
        assert!(!robots_txt("https://demo.thermite.rs", true).contains("Sitemap:"));
    }
}

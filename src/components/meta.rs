//! The `<head>` tags a crawler reads: the share card (Open Graph, Twitter), a public page's
//! title, description and canonical URL, and the structured data behind a rich result.
//!
//! Dioxus writes head elements once, at server render, and hydration leaves them alone: the
//! client mounts the same components so its hydration entries line up with the server's, but
//! creates nothing. The values therefore only have to be right on the server — the only side
//! that can read the instance configuration anyway. A client-side navigation later *appends* a
//! second set (with relative URLs, since the client has no origin) beside the first: Dioxus
//! cannot replace head tags it has written, and nothing reads a head after navigation anyway.

use dioxus::prelude::*;

/// The share image's pixel size, declared in the tags so a scraper can lay the card out before
/// it has fetched the image. `server::og` serves the file; its tests assert it matches.
pub const SHARE_IMAGE_WIDTH: u32 = 1200;
pub const SHARE_IMAGE_HEIGHT: u32 = 630;

/// Bump on every redraw of the cards. Facebook, LinkedIn and X keep a shared link's card for
/// weeks and copy the image into their own CDN keyed on its URL, so a redrawn card only reaches
/// a link that was already shared once its URL changes — a shorter cache on the file itself
/// would change nothing.
pub const SHARE_IMAGE_VERSION: u32 = 1;

/// What the product is, in one sentence: the landing page's description and the fallback for
/// any page that has none of its own.
pub const SITE_DESCRIPTION: &str = "Self-hosted, Sentry-compatible error tracking. Unmodified SDKs report in, issues group in your Postgres, and your coding agent diagnoses them over MCP.";

/// The instance's public origin: scrapers require absolute URLs and silently drop a relative
/// `og:image`. A trailing slash is trimmed so paths can be appended as they are.
#[cfg(feature = "server")]
fn origin() -> String {
    crate::server::state::AppState::global()
        .config
        .base_url
        .trim_end_matches('/')
        .to_string()
}

/// Never reaches the head (see the module doc), so nothing is looked up.
#[cfg(not(feature = "server"))]
fn origin() -> String {
    String::new()
}

/// The site-wide share card, rendered once at the app root so every URL previews with it.
/// `/og.png` serves the demo card on the demo instance, so these tags are identical on every
/// instance and only the server knows which card it hands out.
#[component]
pub fn ShareMeta() -> Element {
    let image = format!("{}/og.png?v={SHARE_IMAGE_VERSION}", origin());
    let alt = "Thermite: the error tracker your agent works in";
    rsx! {
        document::Meta { property: "og:site_name", content: "Thermite" }
        document::Meta { property: "og:type", content: "website" }
        document::Meta { property: "og:locale", content: "en_US" }
        document::Meta { property: "og:image", content: image }
        document::Meta { property: "og:image:type", content: "image/png" }
        document::Meta { property: "og:image:width", content: "{SHARE_IMAGE_WIDTH}" }
        document::Meta { property: "og:image:height", content: "{SHARE_IMAGE_HEIGHT}" }
        document::Meta { property: "og:image:alt", content: alt }
        // X falls back to og:image, og:title and og:description, but documents no fallback
        // for the alt text, and the card type has no Open Graph equivalent at all.
        document::Meta { name: "twitter:card", content: "summary_large_image" }
        document::Meta { name: "twitter:image:alt", content: alt }
    }
}

/// Structured data for the pages a search engine may show a rich result for. Script bodies are
/// written verbatim, so `</` is escaped the way JSON allows and no value can close the tag.
#[component]
pub fn JsonLd(data: serde_json::Value) -> Element {
    let json = data.to_string().replace("</", "<\\/");
    rsx! {
        document::Script { r#type: "application/ld+json", {json} }
    }
}

/// The product as schema.org describes software. The `@id` is what tells a consumer that the
/// landing and pricing pages describe one thing, not two products called Thermite.
pub fn software_application(
    path: &str,
    description: &str,
    offers: serde_json::Value,
) -> serde_json::Value {
    let origin = origin();
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "SoftwareApplication",
        "@id": format!("{origin}/#software"),
        "name": "Thermite",
        "url": format!("{origin}{path}"),
        "description": description,
        "applicationCategory": "DeveloperApplication",
        "operatingSystem": "Linux",
        "image": format!("{origin}/og.png?v={SHARE_IMAGE_VERSION}"),
        "offers": offers,
        "author": {
            "@type": "Person",
            "name": "Hauke Jung",
            "sameAs": [
                "https://github.com/hauju",
                "https://x.com/haukejung",
                "https://www.linkedin.com/in/haukejung/",
            ],
        },
    })
}

/// The founder as schema.org describes a person, for the about page. The `sameAs` links are
/// the ones `software_application` names as author, so the two resolve to one person.
pub fn person(path: &str) -> serde_json::Value {
    let origin = origin();
    serde_json::json!({
        "@context": "https://schema.org",
        "@type": "Person",
        "@id": format!("{origin}/#hauke-jung"),
        "name": "Hauke Jung",
        "jobTitle": "Founder",
        "url": format!("{origin}{path}"),
        "mainEntityOfPage": format!("{origin}{path}"),
        "email": "mail@haukejung.de",
        "description": "Full-stack developer in Germany, creator of Thermite, the Sentry-compatible error tracker that hands every issue to a coding agent.",
        "knowsAbout": [
            "Error tracking",
            "Rust",
            "Model Context Protocol",
            "Full-stack web development",
            "Self-hosted software",
        ],
        "worksFor": { "@type": "Organization", "name": "Thermite", "url": origin },
        "sameAs": [
            "https://github.com/hauju",
            "https://x.com/haukejung",
            "https://www.linkedin.com/in/haukejung/",
        ],
    })
}

/// A plan as an `Offer`. The price specification is what makes it monthly: a bare `price` reads
/// as a one-off purchase.
pub fn offer(name: &str, price: &str, volume: &str) -> serde_json::Value {
    serde_json::json!({
        "@type": "Offer",
        "name": name,
        "price": price,
        "priceCurrency": "USD",
        "description": volume,
        "priceSpecification": {
            "@type": "UnitPriceSpecification",
            "price": price,
            "priceCurrency": "USD",
            "referenceQuantity": { "@type": "QuantitativeValue", "value": 1, "unitCode": "MON" },
        },
    })
}

/// A public page's title, description and URL, for the tab, search results and the share
/// card. Pages without one fall back to the app title and the URL that was fetched. The
/// canonical follows `BASE_URL`, so every instance — the demo included — canonicalises itself.
#[component]
pub fn PageMeta(title: String, description: String, path: String) -> Element {
    let url = format!("{}{path}", origin());
    rsx! {
        document::Title { "{title}" }
        document::Meta { name: "description", content: description.clone() }
        document::Link { rel: "canonical", href: url.clone() }
        document::Meta { property: "og:title", content: title.clone() }
        document::Meta { property: "og:description", content: description }
        document::Meta { property: "og:url", content: url }
    }
}

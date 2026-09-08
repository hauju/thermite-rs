//! `GET /og.png` — the share card (Open Graph / Twitter) for this instance.
//!
//! A fixed path rather than a hashed asset, like the PWA icons, so the tags can name it; the
//! demo instance serves its own card here, which is what lets the tags (`components::meta`) stay
//! identical on every instance. The tags append `?v=<SHARE_IMAGE_VERSION>`, bumped on every
//! redraw, so the file itself can be cached as immutable: the scrapers keep a card for weeks
//! regardless of what this header says, and only a new URL reaches a link already shared.

use axum::Router;
use axum::http::header;
use axum::routing::get;

const CARD: &[u8] = include_bytes!("../../assets/og.png");
const DEMO_CARD: &[u8] = include_bytes!("../../assets/og-demo.png");

pub fn og_router(demo: bool) -> Router {
    let bytes = if demo { DEMO_CARD } else { CARD };
    Router::new().route(
        "/og.png",
        get(move || async move {
            (
                [
                    (header::CONTENT_TYPE, "image/png"),
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                ],
                bytes,
            )
        }),
    )
}

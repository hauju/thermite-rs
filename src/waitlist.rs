//! The hosted-signup waitlist: the server functions and the form.
//!
//! While `THERMITE_WAITLIST` is set, registration is closed and the landing and pricing pages
//! collect addresses instead of sending people to a login that would refuse them. Self-hosting,
//! the docs and the live demo stay exactly as they are — the waitlist gates the hosted signup,
//! not the product.

use dioxus::prelude::*;

#[cfg(feature = "server")]
use crate::models::AppError;

/// Whether hosted signup is behind the waitlist. No session: the landing page asks.
#[post("/api/waitlist/open")]
pub async fn waitlist_open() -> Result<bool, ServerFnError> {
    Ok(crate::server::state::AppState::global().config.waitlist)
}

/// The one unauthenticated write on the marketing pages, so it carries its own quota: five a
/// minute per IP, counted in PostgreSQL like the auth routes. Errors are flat except validation,
/// which describes the caller's own input.
#[post("/api/waitlist", headers: axum::http::HeaderMap, peer: axum::extract::ConnectInfo<std::net::SocketAddr>)]
pub async fn join_waitlist(email: String) -> Result<(), ServerFnError> {
    // Before anything else: a malformed address should not cost a round trip.
    let email = crate::models::waitlist::validate_email(&email)
        .map_err(|e| ServerFnError::from(AppError::Validation(e)))?;

    let state = crate::server::state::AppState::global();
    if !state.config.waitlist {
        return Err(ServerFnError::from(AppError::NotFound));
    }

    let key = crate::server::security::rate_limit_key(
        &headers,
        Some(peer.0),
        state.config.trust_proxy_headers,
    );
    let limiter = crate::server::rate_limit::SharedRateLimiter::per_minute(
        state.db.pool.clone(),
        "waitlist",
        5,
    );
    // Fail open on a database error, as the other shared limiters do: the insert below needs
    // the same database and will fail on its own.
    if !limiter.check(&key).await.unwrap_or(true) {
        return Err(ServerFnError::from(AppError::LimitExceeded(
            "too many attempts, please try again in a minute".into(),
        )));
    }

    crate::server::waitlist::join(&state.db, &email).await?;
    // The address stays out of the log: it is the one piece of personal data this endpoint
    // handles, and the row is where it belongs.
    tracing::info!("waitlist signup");
    Ok(())
}

/// Email capture: one field, one button, and a line of confirmation afterwards.
#[component]
pub fn WaitlistForm() -> Element {
    let mut email = use_signal(String::new);
    let mut pending = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut joined = use_signal(|| false);

    let submit = move |evt: Event<FormData>| {
        evt.prevent_default();
        if pending() {
            return;
        }
        // The same check the server runs, so a typo is answered before the round trip.
        let value = match crate::models::waitlist::validate_email(&email()) {
            Ok(value) => value,
            Err(e) => {
                error.set(Some(e));
                return;
            }
        };
        spawn(async move {
            pending.set(true);
            error.set(None);
            match join_waitlist(value).await {
                Ok(()) => joined.set(true),
                Err(e) => error.set(Some(e.to_string())),
            }
            pending.set(false);
        });
    };

    rsx! {
        if joined() {
            div { class: "alert alert-success rounded-xl w-full max-w-md", role: "status",
                span { "You're on the list. We'll email you when hosted signup opens." }
            }
        } else {
            form { class: "flex flex-col sm:flex-row gap-2 w-full max-w-md", onsubmit: submit,
                input {
                    r#type: "email",
                    name: "email",
                    autocomplete: "email",
                    required: true,
                    placeholder: "you@example.com",
                    class: "input flex-1 rounded-xl",
                    value: "{email}",
                    oninput: move |e| email.set(e.value()),
                }
                button {
                    r#type: "submit",
                    class: "btn btn-primary btn-strong rounded-xl",
                    disabled: pending(),
                    if pending() { "Adding you…" } else { "Join the waitlist" }
                }
            }
            if let Some(e) = error() {
                p { class: "text-error text-sm mt-3", role: "alert", "{e}" }
            }
        }
    }
}

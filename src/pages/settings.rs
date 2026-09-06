use dioxus::prelude::*;

use crate::UserAuthState;
use crate::api_keys::ApiKeysCard;

/// Settings page — user account settings.
#[component]
pub fn Settings() -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();

    let (email, username) = match &*user_auth.read() {
        UserAuthState::Authenticated(data) => (data.email.clone(), data.username.clone()),
        _ => (String::new(), String::new()),
    };

    rsx! {
        div { class: "max-w-2xl",
            h1 { class: "text-3xl font-bold mb-2", "Settings" }
            p { class: "text-base-content/70 mb-8", "Manage your account settings." }

            // Profile section
            div { class: "card bg-base-200 border border-base-300",
                div { class: "card-body",
                    h2 { class: "card-title text-lg mb-4", "Profile" }

                    // Read-only values render as text: a disabled DaisyUI input loses its border
                    // and matches the card fill, so it looked like a label over an invisible box.
                    dl { class: "flex flex-col gap-3 text-sm",
                        div {
                            dt { class: "text-xs uppercase tracking-wide text-base-content/50", "Username" }
                            dd { class: "font-medium mt-0.5", "{username}" }
                        }
                        div {
                            dt { class: "text-xs uppercase tracking-wide text-base-content/50", "Email" }
                            dd { class: "font-medium mt-0.5", "{email}" }
                        }
                    }
                }
            }

            ApiKeysCard {}
        }
    }
}

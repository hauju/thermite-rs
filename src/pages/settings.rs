use dioxus::prelude::*;

use crate::UserAuthState;
use crate::api_keys::ApiKeysCard;
use crate::components::toast::{ToastLevel, show_toast};
use crate::errors_data::set_display_name;

/// Settings page — user account settings.
#[component]
pub fn Settings() -> Element {
    let user_auth = use_context::<Signal<UserAuthState>>();

    let (email, username) = match &*user_auth.read() {
        UserAuthState::Authenticated(data) => (data.email.clone(), data.username.clone()),
        _ => (String::new(), String::new()),
    };

    rsx! {
        div { class: "max-w-2xl mx-auto",
            h1 { class: "text-3xl font-bold mb-2", "Settings" }
            p { class: "text-base-content/70 mb-8", "Manage your account settings." }

            ProfileCard { email, username }

            ApiKeysCard {}
        }
    }
}

/// The display name, and the email beside it as read-only text.
#[component]
fn ProfileCard(email: String, username: String) -> Element {
    // The trigger the shell's `/api/me` resource watches: a saved name has to reach the header
    // and the avatar initial too, not just this field.
    let mut refresh = use_context::<Signal<auth::UserDataRefreshTrigger>>();
    let mut name = use_signal(|| username.clone());
    let mut saving = use_signal(|| false);

    let save = move || async move {
        let value = name().trim().to_string();
        if value.is_empty() || saving() {
            return;
        }
        saving.set(true);
        let result = set_display_name(value).await;
        saving.set(false);

        match result {
            Ok(saved) => {
                name.set(saved);
                refresh.write().0 += 1;
                show_toast("Name saved", ToastLevel::Success);
            }
            Err(e) => show_toast(format!("Could not save: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-lg", "Profile" }

                form {
                    class: "flex flex-wrap gap-2 items-end",
                    onsubmit: move |e| {
                        e.prevent_default();
                        async move { save().await }
                    },
                    label { class: "flex flex-col flex-1 min-w-48",
                        span { class: "text-xs mb-1", "Display name" }
                        input {
                            class: "input input-sm w-full",
                            value: "{name}",
                            maxlength: "64",
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    button {
                        class: "btn btn-sm btn-outline",
                        r#type: "submit",
                        disabled: saving() || name().trim().is_empty(),
                        if saving() {
                            span { class: "loading loading-spinner loading-xs" }
                        }
                        "Save"
                    }
                }
                p { class: "text-xs text-base-content/60",
                    "This is the name an issue's activity and your notes are signed with."
                }

                // Read-only values render as text: a disabled DaisyUI input loses its border
                // and matches the card fill, so it looked like a label over an invisible box.
                div {
                    div { class: "text-xs uppercase tracking-wide text-base-content/50", "Email" }
                    div { class: "font-medium mt-0.5 text-sm", "{email}" }
                    p { class: "text-xs text-base-content/60 mt-1",
                        "Fixed — it is the address your sign-in is matched against."
                    }
                }
            }
        }
    }
}

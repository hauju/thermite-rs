//! Per-project settings: display name, repository, alert routing, component DSN keys, and
//! deletion.
//!
//! Everything that *configures* a project lives here; the projects list only shows what you
//! copy into an SDK.

use dioxus::prelude::*;

use crate::components::toast::{ToastLevel, show_toast};
use crate::errors_data::{
    create_project_key, delete_project, delete_project_key, get_project, rename_project,
    set_alert_routing, set_repo_url,
};
use crate::models::errors::ProjectSummary;
use crate::routes::Route;
use crate::version::load_error;

#[component]
pub fn ProjectSettings(slug: String) -> Element {
    let mut project = use_resource({
        let slug = slug.clone();
        move || {
            let slug = slug.clone();
            async move { get_project(slug).await }
        }
    });

    rsx! {
        div { class: "max-w-2xl",
            match &*project.read_unchecked() {
                Some(Ok(data)) => rsx! {
                    div { class: "mb-6",
                        div { class: "text-xs text-base-content/50 mb-1",
                            Link {
                                to: Route::Projects {},
                                class: "hover:text-primary",
                                "Projects"
                            }
                            " / {data.slug}"
                        }
                        h1 { class: "text-2xl font-bold", "{data.name}" }
                        p { class: "text-base-content/60", "Project settings" }
                    }
                    div { class: "flex flex-col gap-4",
                        // The heading above renders the fetched name, so a rename has to
                        // refetch or the page keeps showing the old one behind a success
                        // toast — which reads as a failed save. `restart` leaves the
                        // existing value in place, so the cards' own signals (the name
                        // being edited, the key list) are not clobbered mid-edit.
                        GeneralCard {
                            project: data.clone(),
                            on_renamed: move |()| project.restart(),
                        }
                        RepositoryCard { project: data.clone() }
                        AlertRoutingCard { project: data.clone() }
                        KeysCard { project: data.clone() }
                        DangerZone { project: data.clone() }
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "alert alert-error mb-4", {load_error("Could not load the project", e)} }
                    Link { to: Route::Projects {}, class: "btn btn-sm btn-outline", "Back to projects" }
                },
                None => rsx! {
                    div { class: "flex flex-col gap-4",
                        div { class: "skeleton h-8 w-64" }
                        for _ in 0..3 {
                            div { class: "skeleton h-32" }
                        }
                    }
                },
            }
        }
    }
}

/// Display name. The slug stays fixed — it lives in every configured DSN and API path.
#[component]
fn GeneralCard(project: ProjectSummary, on_renamed: EventHandler<()>) -> Element {
    let slug = use_signal(|| project.slug.clone());
    let mut name = use_signal(|| project.name.clone());
    let mut saving = use_signal(|| false);

    let save = move || async move {
        if name().trim().is_empty() {
            return;
        }
        saving.set(true);
        let result = rename_project(slug(), name().trim().to_string()).await;
        saving.set(false);

        match result {
            Ok(()) => {
                show_toast("Name saved", ToastLevel::Success);
                on_renamed.call(());
            }
            Err(e) => show_toast(format!("Could not rename: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base", "General" }
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
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    button {
                        class: "btn btn-sm btn-primary",
                        r#type: "submit",
                        disabled: saving() || name().trim().is_empty(),
                        if saving() {
                            span { class: "loading loading-spinner loading-xs" }
                        }
                        "Save"
                    }
                }
                // The slug is shown, not offered as a disabled input: a disabled DaisyUI
                // input loses its border and matches the card fill, so it rendered as a
                // label floating over an invisible box — indistinguishable from a bug.
                div { class: "text-xs text-base-content/60",
                    "Slug "
                    code { class: "bg-base-300 rounded px-1.5 py-0.5 font-mono break-all", "{slug}" }
                    " is fixed — it is part of every DSN already configured in an SDK."
                }
            }
        }
    }
}

/// The repository a triaging agent opens its pull request against.
#[component]
fn RepositoryCard(project: ProjectSummary) -> Element {
    let slug = use_signal(|| project.slug.clone());
    let mut repo = use_signal(|| project.repo_url.clone().unwrap_or_default());
    let mut saving = use_signal(|| false);

    let save = move || async move {
        saving.set(true);
        let result = set_repo_url(slug(), repo().trim().to_string()).await;
        saving.set(false);

        match result {
            Ok(()) => show_toast("Repository saved", ToastLevel::Success),
            Err(e) => show_toast(
                format!("Could not save the repository: {e}"),
                ToastLevel::Error,
            ),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base", "Repository" }
                p { class: "text-xs text-base-content/60",
                    "Handed to an agent with the triage work, so it can open a pull request \
                     instead of stopping at a written diagnosis. Thermite only stores the link — \
                     it holds no credential for this repository and never calls it."
                }
                form {
                    class: "flex flex-wrap gap-2 items-end",
                    onsubmit: move |e| {
                        e.prevent_default();
                        async move { save().await }
                    },
                    label { class: "flex flex-col flex-1 min-w-48",
                        span { class: "text-xs mb-1", "Repository URL" }
                        input {
                            class: "input input-sm w-full",
                            placeholder: "https://github.com/owner/repo",
                            value: "{repo}",
                            oninput: move |e| repo.set(e.value()),
                        }
                    }
                    button {
                        class: "btn btn-sm btn-outline",
                        r#type: "submit",
                        disabled: saving(),
                        if saving() {
                            span { class: "loading loading-spinner loading-xs" }
                        }
                        "Save"
                    }
                }
            }
        }
    }
}

/// Where this project's alerts go, overriding the instance-wide recipients.
#[component]
fn AlertRoutingCard(project: ProjectSummary) -> Element {
    let slug = use_signal(|| project.slug.clone());
    let mut email = use_signal(|| project.alert_email.clone().unwrap_or_default());
    let mut webhook = use_signal(|| project.alert_webhook.clone().unwrap_or_default());
    let mut saving = use_signal(|| false);

    let save = move || async move {
        saving.set(true);
        let result = set_alert_routing(
            slug(),
            email().trim().to_string(),
            webhook().trim().to_string(),
        )
        .await;
        saving.set(false);

        match result {
            Ok(()) => show_toast("Alert routing saved", ToastLevel::Success),
            Err(e) => show_toast(
                format!("Could not save alert routing: {e}"),
                ToastLevel::Error,
            ),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base", "Alert routing" }
                p { class: "text-xs text-base-content/60",
                    "Overrides the instance-wide recipients for this project. Blank falls back to them."
                }
                form {
                    class: "flex flex-wrap gap-2 items-end",
                    onsubmit: move |e| {
                        e.prevent_default();
                        async move { save().await }
                    },
                    label { class: "flex flex-col flex-1 min-w-48",
                        span { class: "text-xs mb-1", "Email recipients (comma-separated)" }
                        input {
                            class: "input input-sm w-full",
                            placeholder: "blank = instance default",
                            value: "{email}",
                            oninput: move |e| email.set(e.value()),
                        }
                    }
                    label { class: "flex flex-col flex-1 min-w-48",
                        span { class: "text-xs mb-1", "Webhook URL" }
                        input {
                            class: "input input-sm w-full",
                            placeholder: "blank = instance default",
                            value: "{webhook}",
                            oninput: move |e| webhook.set(e.value()),
                        }
                    }
                    button {
                        class: "btn btn-sm btn-outline",
                        r#type: "submit",
                        disabled: saving(),
                        if saving() {
                            span { class: "loading loading-spinner loading-xs" }
                        }
                        "Save"
                    }
                }
            }
        }
    }
}

/// The default DSN plus the labeled component keys, with mint and revoke.
#[component]
fn KeysCard(project: ProjectSummary) -> Element {
    let slug = use_signal(|| project.slug.clone());
    let dsn = project.dsn.clone();
    let mut keys = use_signal(|| project.keys.clone());
    let mut label = use_signal(String::new);
    let mut creating = use_signal(|| false);

    let submit = move || async move {
        if label().trim().is_empty() {
            return;
        }
        creating.set(true);
        let result = create_project_key(slug(), label().trim().to_string()).await;
        creating.set(false);

        match result {
            Ok(key) => {
                show_toast(
                    format!("Created component key {}", key.label),
                    ToastLevel::Success,
                );
                label.set(String::new());
                keys.write().push(key);
            }
            Err(e) => show_toast(format!("Could not create key: {e}"), ToastLevel::Error),
        }
    };

    // The row is only removed once the server confirms, so without a guard a second
    // click re-sends the same revoke; the first succeeds, the second finds nothing and
    // reports "not found" for a key the user just watched disappear.
    let mut revoking = use_signal(|| false);

    let revoke = move |target: String| async move {
        if revoking() {
            return;
        }
        revoking.set(true);
        let result = delete_project_key(slug(), target.clone()).await;
        revoking.set(false);

        match result {
            Ok(()) => {
                show_toast(format!("Revoked key {target}"), ToastLevel::Success);
                keys.write().retain(|k| k.label != target);
            }
            Err(e) => show_toast(format!("Could not revoke key: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-base-300",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base", "DSN keys" }
                div {
                    div { class: "text-xs uppercase tracking-wide text-base-content/50 mb-1", "Default DSN" }
                    code { class: "block bg-base-300 rounded px-3 py-2 text-xs break-all font-mono",
                        "{dsn}"
                    }
                }
                div {
                    div { class: "text-xs uppercase tracking-wide text-base-content/50 mb-1",
                        "Component DSNs"
                    }
                    p { class: "text-xs text-base-content/60 mb-2",
                        "One per part of the product ('worker', 'saas'). Events through one carry "
                        code { "component: <label>" }
                        " as a filterable tag. Revoking stops the key authenticating; ingested events keep their tag."
                    }
                    div { class: "flex flex-col gap-1.5",
                        for key in keys().iter().cloned() {
                            div {
                                key: "{key.label}",
                                class: "flex items-center gap-2",
                                span { class: "badge badge-neutral badge-sm font-mono shrink-0 w-28 justify-start truncate", "{key.label}" }
                                code { class: "flex-1 bg-base-300 rounded px-3 py-1.5 text-xs break-all font-mono",
                                    "{key.dsn}"
                                }
                                button {
                                    class: "btn btn-xs btn-ghost text-error shrink-0",
                                    disabled: revoking(),
                                    // Every one of these buttons reads "Revoke"; without the
                                    // label a screen reader announces an undifferentiated list.
                                    "aria-label": "Revoke key {key.label}",
                                    onclick: move |_| {
                                        let target = key.label.clone();
                                        async move { revoke(target).await }
                                    },
                                    "Revoke"
                                }
                            }
                        }
                        form {
                            class: "flex gap-2",
                            onsubmit: move |e| {
                                e.prevent_default();
                                async move { submit().await }
                            },
                            input {
                                class: "input input-xs w-44",
                                placeholder: "component (e.g. worker)",
                                value: "{label}",
                                oninput: move |e| label.set(e.value()),
                            }
                            button {
                                class: "btn btn-xs btn-outline",
                                r#type: "submit",
                                disabled: creating() || label().trim().is_empty(),
                                if creating() {
                                    span { class: "loading loading-spinner loading-xs" }
                                }
                                "Add key"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Deletion, gated on typing the slug — a click on the wrong card must not be able to do this.
#[component]
fn DangerZone(project: ProjectSummary) -> Element {
    let slug = use_signal(|| project.slug.clone());
    let mut confirm = use_signal(String::new);
    let mut deleting = use_signal(|| false);
    let nav = use_navigator();

    let armed = move || confirm() == slug();

    let delete = move || async move {
        if !armed() {
            return;
        }
        deleting.set(true);
        let result = delete_project(slug()).await;
        deleting.set(false);

        match result {
            Ok(()) => {
                show_toast(format!("Deleted {}", slug()), ToastLevel::Success);
                // `replace`, not `push`: this route no longer resolves, so leaving it in
                // history means Back lands on a 404 for the project just deleted.
                nav.replace(Route::Projects {});
            }
            Err(e) => show_toast(format!("Could not delete: {e}"), ToastLevel::Error),
        }
    };

    rsx! {
        div { class: "card bg-base-200 border border-error/40",
            div { class: "card-body gap-3",
                h2 { class: "card-title text-base text-error", "Danger zone" }
                p { class: "text-xs text-base-content/60",
                    "Deletes the project with all its issues, events, monitors and keys, and its DSNs "
                    "stop authenticating immediately. This cannot be undone."
                }
                form {
                    class: "flex flex-wrap gap-2 items-end",
                    onsubmit: move |e| {
                        e.prevent_default();
                        async move { delete().await }
                    },
                    label { class: "flex flex-col flex-1 min-w-48",
                        span { class: "text-xs mb-1",
                            "Type "
                            span { class: "font-mono font-semibold", "{slug}" }
                            " to confirm"
                        }
                        input {
                            class: "input input-sm w-full font-mono",
                            value: "{confirm}",
                            oninput: move |e| confirm.set(e.value()),
                        }
                    }
                    button {
                        class: "btn btn-sm btn-error",
                        r#type: "submit",
                        disabled: deleting() || !armed(),
                        if deleting() {
                            span { class: "loading loading-spinner loading-xs" }
                        }
                        "Delete project"
                    }
                }
            }
        }
    }
}

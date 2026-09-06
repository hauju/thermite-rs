use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::LdCopy};

use crate::components::toast::{ToastLevel, show_toast};

/// Copies a DSN to the clipboard — the one action this page exists for, so it should
/// not require hand-selecting a long URL.
#[component]
pub fn CopyDsn(dsn: String, label: String) -> Element {
    // The closure takes its own copy so the aria-label can keep borrowing `label`.
    let toast_label = label.clone();
    rsx! {
        button {
            class: "btn btn-sm btn-ghost btn-square shrink-0",
            "aria-label": "Copy DSN for {label}",
            title: "Copy DSN",
            onclick: move |_| {
                // DSNs are URLs, so the single-quoted JS literal cannot be broken by one.
                let _ = document::eval(&format!("navigator.clipboard.writeText('{dsn}')"));
                show_toast(format!("DSN for {toast_label} copied"), ToastLevel::Success);
            },
            Icon { icon: LdCopy, width: 14, height: 14 }
        }
    }
}

use dioxus::prelude::*;

/// One question and its answer. Shared by the landing and pricing pages: the two FAQ sections
/// answer different questions, but a reader moving between them should not see two designs.
#[component]
pub fn FaqCard(question: &'static str, answer: &'static str) -> Element {
    rsx! {
        div { class: "card card-elevated bg-base-200 h-full",
            div { class: "card-body gap-2",
                h3 { class: "card-title text-base", "{question}" }
                p { class: "text-base-content/60 text-sm leading-relaxed", "{answer}" }
            }
        }
    }
}

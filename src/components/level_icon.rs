//! An event level as an icon, for the list rows where it competes with everything else.

use dioxus::prelude::*;
use dioxus_free_icons::{Icon, icons::ld_icons::*};

/// The level of an issue or event, drawn as one icon.
///
/// Lists carry the level beside the triage state, the project and the issue title, and as a
/// filled badge it read as loudly as any of them while saying the same word on nearly every
/// row. The detail page keeps the badge with its text: there is one issue there and room to
/// name it.
///
/// **The shapes differ, not just the colours.** Severity is the one thing on the row a
/// red-green deficient reader most needs, and five coloured dots would carry none of it — the
/// same reason the light palette is spaced on lightness. It is also what separates `fatal`
/// from `error`, which share a colour because both are broken and only one is worth crossing
/// the room for.
///
/// The word rides along in `title` and `aria-label`: an icon nobody can name is a decoration.
#[component]
pub fn LevelIcon(level: String, #[props(default = 16)] size: u32) -> Element {
    // `_` is `debug` and anything an SDK invented; `protocol::event::level` already narrows the
    // wire value to the five, defaulting to `error`.
    let color = match level.as_str() {
        "fatal" | "error" => "text-error",
        "warning" => "text-warning",
        "info" => "text-info",
        _ => "text-base-content/50",
    };

    rsx! {
        span {
            class: "{color} shrink-0 inline-flex",
            title: "{level}",
            "aria-label": "Level: {level}",
            role: "img",
            match level.as_str() {
                // An X, not a second exclamation mark. At 16px the outline barely reads — an
                // octagon and a circle differ by about a pixel — so what separates fatal from
                // error has to be the glyph inside it, which is the one distinction worth
                // seeing across a list: the process died, or it carried on.
                "fatal" => rsx! { Icon { icon: LdOctagonX, width: size, height: size } },
                "error" => rsx! { Icon { icon: LdCircleAlert, width: size, height: size } },
                "warning" => rsx! { Icon { icon: LdTriangleAlert, width: size, height: size } },
                "info" => rsx! { Icon { icon: LdInfo, width: size, height: size } },
                _ => rsx! { Icon { icon: LdBug, width: size, height: size } },
            }
        }
    }
}

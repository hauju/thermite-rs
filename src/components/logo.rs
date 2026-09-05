use dioxus::prelude::*;

/// The gradient definitions for [`ThermiteMark`], rendered exactly once at the app root.
///
/// They are hoisted out of the mark on purpose: every mark referencing `url(#tm-shell)`
/// resolves to the *first* element with that id in the document, and browsers do not paint
/// gradients that live inside a `display:none` subtree. With per-instance defs, the first
/// copy on desktop sat inside the `lg:hidden` mobile navbar — so the sidebar and dashboard
/// marks, gradient-stroked and gradient-filled with no fallback, rendered fully invisible.
/// One defs block in an always-rendered 0x0 SVG cannot be hidden by any layout state.
#[component]
pub fn ThermiteMarkDefs() -> Element {
    rsx! {
        svg {
            xmlns: "http://www.w3.org/2000/svg",
            width: "0",
            height: "0",
            class: "absolute",
            "aria-hidden": "true",
            defs {
                linearGradient {
                    id: "tm-shell",
                    x1: "0",
                    y1: "0",
                    x2: "1",
                    y2: "1",
                    stop { offset: "0", stop_color: "#ffc233" }
                    stop { offset: "1", stop_color: "#e2401f" }
                }
                linearGradient {
                    id: "tm-core",
                    x1: "0",
                    y1: "0",
                    x2: "0",
                    y2: "1",
                    stop { offset: "0", stop_color: "#fffdf7" }
                    stop { offset: "1", stop_color: "#ffb020" }
                }
            }
        }
    }
}

/// The Thermite mark: a molten outer triangle (fire/warning symbol) around a
/// white-hot core — the reaction burns hottest at its centre, so the heat runs
/// inward (gold shell → ember base, white core) rather than the other way round.
///
/// Gradients are absolute colors so the mark reads identically on both themes and
/// against the favicon's transparent background. They are defined once in
/// [`ThermiteMarkDefs`] at the app root — see its doc comment before moving them back.
///
/// **The core is stroked with the shell gradient, not with itself.** Filled and
/// stroked in the same near-white, its top half measured 1.04:1 against the light
/// theme's base — the core silently dissolved into the page and the mark read as
/// an orange scoop with a sheared-off top. The ember rim is the only thing giving
/// it an edge on a light background; do not collapse it back to `tm-core`.
#[component]
pub fn ThermiteMark(#[props(default = 24)] size: u32) -> Element {
    rsx! {
        svg {
            xmlns: "http://www.w3.org/2000/svg",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            "aria-hidden": "true",
            path {
                d: "M10.42 3.9 2.77 17.16a1.83 1.83 0 0 0 1.58 2.74h15.3a1.83 1.83 0 0 0 1.58-2.74L13.58 3.9a1.83 1.83 0 0 0-3.16 0Z",
                stroke: "url(#tm-shell)",
                stroke_width: "1.9",
                stroke_linejoin: "round",
            }
            path {
                d: "M12 10.6l3.55 6.15a.62.62 0 0 1-.54.93H8.99a.62.62 0 0 1-.54-.93Z",
                fill: "url(#tm-core)",
                stroke: "url(#tm-shell)",
                stroke_width: "1.3",
                stroke_linejoin: "round",
            }
        }
    }
}

//! Inline SVG charts for event volume.
//!
//! SVG rather than a charting library: these are bars over a fixed bucket count with no axes,
//! interaction or layout logic, and a chart dependency would be far more code than the shapes.

use dioxus::prelude::*;

/// A compact bar chart sized to sit inside a table row.
///
/// `unit` and `fill` exist because the same shape charts sessions as well as events, and a session
/// bar announced as an error would be both wrong and alarming.
///
/// The default fill is primary, not error: these bars chart *volume*, and severity red is
/// reserved for the badges beside them. With the charts also red, every page was uniformly
/// red and a genuine spike had nothing left to stand out with.
#[component]
pub fn Sparkline(
    counts: Vec<i64>,
    #[props(default = 88)] width: u32,
    #[props(default = "events in the last 24 hours".to_string())] unit: String,
    #[props(default = "fill-primary/70".to_string())] fill: String,
) -> Element {
    let height = 24u32;
    let peak = counts.iter().copied().max().unwrap_or(0);

    if counts.is_empty() {
        return rsx! { div { class: "w-[88px] h-6" } };
    }

    let slot = width as f64 / counts.len() as f64;
    let bar = (slot * 0.7).max(1.0);
    let total: i64 = counts.iter().sum();

    rsx! {
        svg {
            width: "{width}",
            height: "{height}",
            view_box: "0 0 {width} {height}",
            role: "img",
            // Screen readers get the number, not a wall of bars.
            "aria-label": "{total} {unit}",
            for (index , value) in counts.iter().enumerate() {
                {
                    // Square-root scale, not linear. In 24px, one outlier flattens everything else:
                    // a 1-event hour against an 11-event spike is 2px linear — technically visible,
                    // visually noise — so a row with hourly errors reads as empty. sqrt puts it at
                    // ~7px. This is a shape indicator, not a measurement; the rate chart above stays
                    // linear precisely because it *is* the measurement.
                    // A zero bucket still draws a 1px tick. At 0.0 it drew nothing at all, so a
                    // mostly-quiet project rendered as a few marks floating in blank space —
                    // which reads as a broken chart rather than a calm one. The row of ticks
                    // is the baseline; the full-width chart below gets a drawn one instead.
                    let bar_height = if *value == 0 || peak == 0 {
                        1.0
                    } else {
                        ((*value as f64 / peak as f64).sqrt() * (height as f64 - 2.0)).max(1.5)
                    };
                    let x = index as f64 * slot;
                    let y = height as f64 - bar_height;
                    rsx! {
                        rect {
                            x: "{x:.2}",
                            y: "{y:.2}",
                            width: "{bar:.2}",
                            height: "{bar_height:.2}",
                            rx: "0.5",
                            class: if *value == 0 { "fill-base-300".to_string() } else { fill.clone() },
                        }
                    }
                }
            }
        }
    }
}

/// The full-width rate chart at the top of a project, with a baseline, hover labels and a
/// time axis. `resolution` is the stats API's bucket width (`1h` / `1d`) and picks the tick
/// format; all times are UTC, like every other timestamp the dashboard shows.
///
/// `dropped` stacks an error-colored segment on top of the stored count: events ingest turned
/// away (over quota, unsupported, invalid). Empty means "don't draw the overlay" — the shape is
/// shared with charts that have no drop dimension.
#[component]
pub fn RateChart(
    counts: Vec<i64>,
    labels: Vec<String>,
    resolution: String,
    #[props(default = Vec::new())] dropped: Vec<i64>,
) -> Element {
    let height = 120u32;
    // The scale fits the stacked total, or a bucket with drops would overflow the chart.
    let peak = counts
        .iter()
        .enumerate()
        .map(|(i, c)| c + dropped.get(i).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);

    if counts.is_empty() {
        return rsx! {
            div { class: "h-[120px] flex items-center justify-center text-base-content/40 text-sm",
                "No data"
            }
        };
    }

    let slot = 100.0 / counts.len() as f64;
    let bar = slot * 0.72;

    let hourly = resolution == "1h";
    // A handful of ticks, not one per bucket. The 7d window is hourly buckets too (168 of
    // them), so past two days of hourly data the ticks fall on midnights and show dates —
    // evenly stepped indices there would label arbitrary clock times on changing days.
    let ticks: Vec<(usize, String)> = if hourly && counts.len() > 48 {
        labels
            .iter()
            .take(counts.len())
            .enumerate()
            .filter(|(_, l)| l.get(11..16) == Some("00:00"))
            .map(|(i, l)| (i, tick_date(l)))
            .collect()
    } else {
        let step = (counts.len() / 6).max(1);
        labels
            .iter()
            .take(counts.len())
            .enumerate()
            .step_by(step)
            .map(|(i, l)| (i, if hourly { tick_time(l) } else { tick_date(l) }))
            .collect()
    };

    rsx! {
        div {
            div { class: "relative",
                svg {
                    width: "100%",
                    height: "{height}",
                    view_box: "0 0 100 {height}",
                    preserve_aspect_ratio: "none",
                    role: "img",
                    "aria-label": "Events per bucket",
                    for (index , value) in counts.iter().enumerate() {
                        {
                            let drops = dropped.get(index).copied().unwrap_or(0);
                            let bar_height = if *value == 0 {
                                0.0
                            } else if peak == 0 {
                                2.0
                            } else {
                                (*value as f64 / peak as f64 * (height as f64 - 8.0)).max(2.0)
                            };
                            // Stacked above the stored bar, with the same minimum so a single
                            // dropped event in a tall window is still a visible sliver.
                            let drop_height = if drops == 0 || peak == 0 {
                                0.0
                            } else {
                                (drops as f64 / peak as f64 * (height as f64 - 8.0)).max(2.0)
                            };
                            let x = index as f64 * slot;
                            let y = height as f64 - bar_height;
                            let drop_y = y - drop_height;
                            let label = labels.get(index).map(String::as_str).unwrap_or_default();
                            let when = if hourly {
                                format!("{} {}", tick_date(label), tick_time(label))
                            } else {
                                tick_date(label)
                            };
                            rsx! {
                                rect {
                                    x: "{x:.3}",
                                    y: "{y:.3}",
                                    width: "{bar:.3}",
                                    height: "{bar_height:.3}",
                                    class: if *value == 0 { "fill-base-300" } else { "fill-primary/60 hover:fill-primary" },
                                    title { "{when}: {value}" }
                                }
                                if drop_height > 0.0 {
                                    rect {
                                        x: "{x:.3}",
                                        y: "{drop_y:.3}",
                                        width: "{bar:.3}",
                                        height: "{drop_height:.3}",
                                        class: "fill-error/70 hover:fill-error",
                                        title { "{when}: {drops} dropped" }
                                    }
                                }
                            }
                        }
                    }
                }
                // Baseline, so an all-zero chart still reads as a chart rather than blank space.
                div { class: "absolute inset-x-0 bottom-0 border-b border-base-300" }
            }
            div { class: "relative h-4 mt-1",
                for (index , text) in ticks {
                    span {
                        class: "absolute -translate-x-1/2 text-xs text-base-content/40 tabular-nums whitespace-nowrap",
                        style: "left: {(index as f64 + 0.5) * slot:.3}%",
                        "{text}"
                    }
                }
            }
        }
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `2026-08-25T14:00:00+00:00` → `14:00`. Byte slicing, not parsing: the client has no
/// clock-aware chrono, and RFC 3339 is fixed-position ASCII.
fn tick_time(rfc3339: &str) -> String {
    rfc3339.get(11..16).unwrap_or(rfc3339).to_string()
}

/// `2026-08-05T14:00:00+00:00` → `5 Aug`.
fn tick_date(rfc3339: &str) -> String {
    let month = rfc3339
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)));
    let day = rfc3339.get(8..10).map(|d| d.trim_start_matches('0'));
    match (day, month) {
        (Some(day), Some(month)) => format!("{day} {month}"),
        _ => rfc3339.to_string(),
    }
}

#[cfg(test)]
mod tick_tests {
    use super::{tick_date, tick_time};

    #[test]
    fn ticks_slice_rfc3339_without_parsing() {
        assert_eq!(tick_time("2026-08-25T14:00:00+00:00"), "14:00");
        assert_eq!(tick_date("2026-08-05T14:00:00+00:00"), "5 Aug");
        assert_eq!(tick_date("2026-12-31T00:00:00+00:00"), "31 Dec");
        // Garbage stays visible rather than panicking or vanishing.
        assert_eq!(tick_time("nonsense"), "nonsense");
        assert_eq!(tick_date("nonsense"), "nonsense");
    }
}

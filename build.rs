use std::fs;
use std::path::Path;

fn main() {
    dioxus_docs_kit_build::generate_content_map("docs/_nav.json");
    prerender_legal_markdown();
}

/// Renders `assets/legal/*.md` to HTML at build time so the legal pages can
/// `include_str!` the result. Doing it here rather than at runtime keeps the
/// markdown parser out of the WASM bundle and off the request path.
fn prerender_legal_markdown() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let legal_dir = Path::new("assets/legal");
    let files = ["imprint.md", "privacy.md", "terms.md", "cookies.md"];

    for file in &files {
        let md_path = legal_dir.join(file);
        let md_content = fs::read_to_string(&md_path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", md_path.display()));

        // The imprint is plain CommonMark; the cookie policy needs GFM for its
        // tables, and the other two share its option set for consistency.
        let html = if *file == "imprint.md" {
            markdown::to_html(&md_content)
        } else {
            markdown::to_html_with_options(&md_content, &markdown::Options::gfm())
                .unwrap_or_else(|e| panic!("Failed to render {file}: {e}"))
        };

        let html_name = file.replace(".md", ".html");
        let out_path = Path::new(&out_dir).join(html_name);
        fs::write(&out_path, html)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", out_path.display()));

        println!("cargo::rerun-if-changed={}", md_path.display());
    }
}

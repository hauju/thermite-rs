use std::fs;
use std::path::Path;

fn main() {
    dioxus_docs_kit_build::generate_content_map("docs/_nav.json");
    prerender_legal_markdown();
    emit_build_id();
}

/// The commit both binaries are built from, so a client can tell when the server has been
/// redeployed underneath it (`src/version.rs`). Server and WASM client are separate cargo
/// invocations of one tree, so the id must come from the tree and never from the clock — a
/// timestamp would differ between the two and every fresh load would look out of date.
/// `GITHUB_SHA` first, because CI checks out a detached HEAD; `dev` when there is no git at all,
/// which makes both sides agree and the check inert.
fn emit_build_id() {
    println!("cargo::rerun-if-env-changed=GITHUB_SHA");
    // A commit moves the branch ref, not HEAD; watch both so a rebuild after a commit picks it up.
    if Path::new(".git/HEAD").exists() {
        println!("cargo::rerun-if-changed=.git/HEAD");
        if let Some(r) = fs::read_to_string(".git/HEAD")
            .ok()
            .and_then(|head| head.trim().strip_prefix("ref: ").map(str::to_owned))
            .filter(|r| Path::new(".git").join(r).exists())
        {
            println!("cargo::rerun-if-changed=.git/{r}");
        }
    }

    let id = std::env::var("GITHUB_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    let id: String = id.chars().take(12).collect();
    println!("cargo::rustc-env=THERMITE_BUILD={id}");
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

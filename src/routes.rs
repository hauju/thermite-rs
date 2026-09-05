use dioxus::prelude::*;

use crate::components::dashboard_shell::DashboardShell;
use crate::components::navbar::Navbar;
use crate::pages::dashboard::Dashboard;
use crate::pages::docs::{DocsPage, DocsShell};
use crate::pages::home::Home;
use crate::pages::issue_detail::IssueDetail;
use crate::pages::issues::Issues;
use crate::pages::legal::{CookiesPage, ImprintPage, PrivacyPage, TermsPage};
use crate::pages::login::LoginPage;
use crate::pages::playground::Playground;
use crate::pages::pricing::Pricing;
use crate::pages::project_settings::ProjectSettings;
use crate::pages::projects::Projects;
use crate::pages::settings::Settings;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    #[layout(Navbar)]
        #[route("/")]
        Home {},
        #[route("/pricing")]
        Pricing {},
        #[route("/legal/imprint")]
        ImprintPage {},
        #[route("/legal/privacy")]
        PrivacyPage {},
        #[route("/legal/terms")]
        TermsPage {},
        #[route("/legal/cookies")]
        CookiesPage {},
    #[end_layout]

    #[route("/login?:redirect_url")]
    LoginPage { redirect_url: String },

    #[layout(DashboardShell)]
        #[route("/dashboard")]
        Dashboard {},
        #[route("/projects")]
        Projects {},
        #[route("/projects/:slug")]
        Issues { slug: String },
        #[route("/projects/:slug/settings")]
        ProjectSettings { slug: String },
        #[route("/issues/:id")]
        IssueDetail { id: i64 },
        #[route("/playground")]
        Playground {},
        #[route("/settings")]
        Settings {},
    #[end_layout]

    #[layout(DocsShell)]
        #[redirect("/docs", || Route::DocsPage { slug: vec!["getting-started".into(), "introduction".into()] })]
        #[route("/docs/:..slug")]
        DocsPage { slug: Vec<String> },
}

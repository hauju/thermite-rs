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
        // The filters live in the query string so a view survives a reload and can be linked to.
        #[route("/projects/:slug?:..filters")]
        Issues { slug: String, filters: IssueFilters },
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

impl Route {
    /// A project's issue list with the default filters — what every link into a project wants.
    pub fn issues(slug: impl Into<String>) -> Self {
        Route::Issues {
            slug: slug.into(),
            filters: IssueFilters::default(),
        }
    }
}

/// The issue list's filters as they appear in the URL. Each is `None` at its default, so a plain
/// project link carries no query.
///
/// One type for the whole query rather than one route argument per filter, because the router
/// percent-decodes the query string *before* splitting it on `&` — a search for `a & b` would
/// come back as `a `. Owning the parse means owning the encoding too: values are percent-encoded
/// twice on the way out, so the router's single decode leaves `%26` for this side to turn back
/// into `&`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IssueFilters {
    pub status: Option<String>,
    pub sort: Option<String>,
    pub window: Option<String>,
    pub env: Option<String>,
    pub component: Option<String>,
    pub q: Option<String>,
    /// `key:value` — only issues with events carrying this tag. Set by clicking a value in an
    /// issue's tag distribution.
    pub tag: Option<String>,
}

impl IssueFilters {
    fn pairs(&self) -> [(&'static str, &Option<String>); 7] {
        [
            ("status", &self.status),
            ("sort", &self.sort),
            ("window", &self.window),
            ("env", &self.env),
            ("component", &self.component),
            ("q", &self.q),
            ("tag", &self.tag),
        ]
    }
}

/// What has to survive the router's decode: `&` because it separates pairs, `%` because it
/// starts an escape.
const VALUE_SET: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS.add(b'%').add(b'&');

impl std::fmt::Display for IssueFilters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for (key, value) in self.pairs() {
            let Some(value) = value else { continue };
            let once = percent_encoding::utf8_percent_encode(value, VALUE_SET).to_string();
            let twice = percent_encoding::utf8_percent_encode(&once, VALUE_SET);
            write!(f, "{}{key}={twice}", if first { "" } else { "&" })?;
            first = false;
        }
        Ok(())
    }
}

impl From<&str> for IssueFilters {
    fn from(query: &str) -> Self {
        let mut filters = Self::default();
        for pair in query.split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let value = percent_encoding::percent_decode_str(value)
                .decode_utf8_lossy()
                .into_owned();
            let slot = match key {
                "status" => &mut filters.status,
                "sort" => &mut filters.sort,
                "window" => &mut filters.window,
                "env" => &mut filters.env,
                "component" => &mut filters.component,
                "q" => &mut filters.q,
                "tag" => &mut filters.tag,
                _ => continue,
            };
            *slot = Some(value).filter(|v| !v.is_empty());
        }
        filters
    }
}

#[cfg(test)]
mod tests {
    use super::{IssueFilters, Route};

    #[test]
    fn default_filters_add_nothing_to_the_project_url() {
        // The router always writes the `?`; what matters is that nothing follows it.
        assert_eq!(Route::issues("demo").to_string(), "/projects/demo?");
        assert_eq!(
            "/projects/demo".parse::<Route>().unwrap(),
            Route::issues("demo")
        );
    }

    #[test]
    fn filters_round_trip_through_the_query_string() {
        let route = Route::Issues {
            slug: "demo".into(),
            filters: IssueFilters {
                status: Some("resolved".into()),
                window: Some("7d".into()),
                env: Some("production".into()),
                q: Some("100% timed out & a=b #1".into()),
                tag: Some("url:https://x.example/a?b=1".into()),
                ..Default::default()
            },
        };
        let url = route.to_string();
        assert_eq!(url.parse::<Route>().unwrap(), route, "{url}");
    }

    #[test]
    fn an_unknown_or_empty_argument_is_ignored() {
        let route = "/projects/demo?status=&bogus=1&q=x"
            .parse::<Route>()
            .unwrap();
        assert_eq!(
            route,
            Route::Issues {
                slug: "demo".into(),
                filters: IssueFilters {
                    q: Some("x".into()),
                    ..Default::default()
                },
            }
        );
    }
}

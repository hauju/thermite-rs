//! Delivering alerts to humans: email, a webhook, or both.
//!
//! The notifications outbox already records exactly one row per new issue or regression; agents
//! drain it through the triage API. This loop is the other consumer — it polls for undelivered
//! rows (`thermite_core::alerts`) and pushes them to whatever channels are configured:
//!
//! - `THERMITE_ALERT_EMAIL` — comma-separated recipients, sent through the same SMTP settings as
//!   the rest of the application.
//! - `THERMITE_ALERT_WEBHOOK` — a URL POSTed one JSON object per alert, which is enough for
//!   Slack/Discord/ntfy-style receivers behind a small adapter.
//! - A project's own `alert_email` / `alert_webhook`, when set, replace the two above for that
//!   project's alerts — routing, not fan-out.
//!
//! A row is marked delivered only after every configured channel succeeded, so delivery is
//! at-least-once: a crash or an unreachable receiver re-sends on a later tick rather than losing
//! the alert. Each channel's success is recorded separately, so retrying a failed webhook does
//! not re-send the email that already went out; rows claim a lease first (one replica delivers,
//! not all of them), retries back off exponentially, and a row that keeps failing is
//! dead-lettered with a loud log line instead of blocking the queue head forever. The loop always
//! runs — project routing can appear at runtime — and while nothing is configured anywhere the
//! backlog floor rides with the clock, so enabling alerting later starts clean.

use serde_json::json;
use smtp::AsyncSmtpClient;
use sqlx::PgPool;
use thermite_core::alerts::{Alert, Channel};

use crate::server::state::AppState;

/// How often undelivered alerts are looked for. Alerts are for humans; a minute of latency is
/// invisible next to the time it takes anyone to react.
const POLL_SECONDS: u64 = 60;
/// Alerts per tick. A pathological burst is drained over a few ticks instead of one giant one.
const BATCH: i64 = 50;
/// Attempts before a row is dead-lettered. With the capped exponential backoff this spans about
/// nine hours of trying — plenty for a receiver restart, finite for a permanent rejection.
const MAX_ATTEMPTS: i32 = 10;
/// Ceiling on one SMTP conversation (including the client's internal retries). Without it, a
/// host that neither answers nor resets — a dropped-packet firewall — would hang the whole alert
/// loop indefinitely and silently.
const SMTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The delivery machinery plus the instance-wide default channels. Per-project overrides on the
/// claimed alert replace the defaults for that alert (routing, not fan-out).
struct Sink {
    global_emails: Vec<smtp::Mailbox>,
    smtp: Option<smtp::AsyncSmtpClientImpl>,
    global_webhook: Option<String>,
    http: reqwest::Client,
    base_url: String,
}

/// Parse a comma-separated recipient list, logging (and skipping) what does not parse.
fn parse_mailboxes(raw: &str) -> Vec<smtp::Mailbox> {
    raw.split(',')
        .filter_map(|addr| {
            let addr = addr.trim();
            if addr.is_empty() {
                return None;
            }
            match addr.parse() {
                Ok(mailbox) => Some(mailbox),
                Err(error) => {
                    tracing::error!(%error, addr, "ignoring unparseable alert recipient");
                    None
                }
            }
        })
        .collect()
}

impl Sink {
    /// Always builds: even with no global channel, a project can carry its own routing, so the
    /// machinery has to exist. Whether anything is deliverable is decided per alert.
    fn build(state: &AppState) -> Self {
        let global_emails =
            parse_mailboxes(state.config.alert_email.as_deref().unwrap_or_default());

        let config = smtp::SmtpConfig {
            from: state.config.smtp_from.clone(),
            host: state.config.smtp_host.clone(),
            port: state.config.smtp_port,
            user: state.secrets.smtp_user.clone(),
            password: state.secrets.smtp_password.clone(),
            security: state.config.smtp_security,
        };
        let smtp = match smtp::AsyncSmtpClientImpl::new(config) {
            Ok(client) => Some(client),
            Err(error) => {
                tracing::error!(%error, "alert email disabled: SMTP client failed to build");
                None
            }
        };

        Self {
            global_emails,
            smtp,
            global_webhook: state.config.alert_webhook.clone(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            base_url: state.config.base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Whether any alert could be delivered with the instance-wide configuration alone. Drives
    /// the backlog floor and the claim filter — not per-alert channel resolution.
    fn globally_configured(&self) -> bool {
        (self.smtp.is_some() && !self.global_emails.is_empty()) || self.global_webhook.is_some()
    }

    /// Attempts every configured channel that has not already succeeded, concurrently — one slow
    /// channel must not delay the other. `None` per channel means "nothing to do" (unconfigured,
    /// or done on an earlier attempt); `Some(ok)` is this attempt's outcome.
    async fn deliver(&self, alert: &Alert) -> Delivery {
        let recipients = match &alert.project_alert_email {
            Some(overridden) => parse_mailboxes(overridden),
            None => self.global_emails.clone(),
        };
        let webhook_url = alert
            .project_alert_webhook
            .as_deref()
            .or(self.global_webhook.as_deref());

        let email = async {
            let smtp = self.smtp.as_ref()?;
            if alert.email_done || recipients.is_empty() {
                return None;
            }

            let mut ok = true;
            for mailbox in &recipients {
                let email = smtp::Email::builder(mailbox.clone())
                    .subject(subject(alert))
                    .body(email_body(alert, &self.base_url))
                    .build();
                match tokio::time::timeout(SMTP_TIMEOUT, smtp.send_email(email)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, alert = alert.id, "alert email failed; will retry");
                        ok = false;
                    }
                    Err(_) => {
                        tracing::warn!(alert = alert.id, "alert email timed out; will retry");
                        ok = false;
                    }
                }
            }
            Some(ok)
        };

        let webhook = async {
            let url = webhook_url?;
            if alert.webhook_done {
                return None;
            }

            let ok = self
                .http
                .post(url)
                .json(&payload(alert, &self.base_url))
                .send()
                .await
                .map(|res| res.status().is_success())
                .unwrap_or(false);
            if !ok {
                tracing::warn!(alert = alert.id, "alert webhook failed; will retry");
            }
            Some(ok)
        };

        let (email, webhook) = tokio::join!(email, webhook);
        Delivery { email, webhook }
    }
}

/// Per-channel outcome of one delivery attempt.
struct Delivery {
    email: Option<bool>,
    webhook: Option<bool>,
}

impl Delivery {
    /// True when nothing remains undelivered — every configured channel has succeeded, now or
    /// on an earlier attempt.
    fn complete(&self) -> bool {
        self.email.unwrap_or(true) && self.webhook.unwrap_or(true)
    }
}

/// One poll: claim what is deliverable, attempt it, record what happened. Returns how many rows
/// were fully delivered.
async fn tick(db: &PgPool, sink: &Sink) -> Result<usize, thermite_core::AppError> {
    let mut delivered = 0;

    // While nothing is configured anywhere, the floor rides with the clock — so configuring
    // alerting later (globally or on one project) starts from that moment, not from a backlog.
    let globally_configured = sink.globally_configured();
    thermite_core::alerts::advance_floor_while_unconfigured(db, globally_configured).await?;

    for alert in thermite_core::alerts::claim(db, BATCH, globally_configured).await? {
        let outcome = sink.deliver(&alert).await;

        // Channel successes are recorded even when the row as a whole failed, so the retry only
        // re-sends what actually needs re-sending.
        if outcome.email == Some(true) {
            thermite_core::alerts::mark_channel(db, alert.id, Channel::Email).await?;
        }
        if outcome.webhook == Some(true) {
            thermite_core::alerts::mark_channel(db, alert.id, Channel::Webhook).await?;
        }

        if outcome.complete() {
            thermite_core::alerts::mark_alerted(db, alert.id).await?;
            delivered += 1;
        } else if thermite_core::alerts::record_failure(db, alert.id, MAX_ATTEMPTS).await? {
            tracing::error!(
                alert = alert.id,
                issue = alert.issue_id,
                title = %alert.title,
                "alert delivery abandoned after {MAX_ATTEMPTS} attempts — nobody was told"
            );
        }
    }

    Ok(delivered)
}

/// Polls forever. Failures are logged and retried on the next tick — alerting being down is bad,
/// but taking ingest down with it would be worse.
///
/// Spawned even with no global channel configured: projects can carry their own routing, set at
/// runtime, so the loop has to be there to notice.
pub fn spawn(state: AppState) {
    let sink = Sink::build(&state);

    tracing::info!(
        global_email_recipients = sink.global_emails.len(),
        global_webhook = sink.global_webhook.is_some(),
        "alert loop running (projects may add their own routing)"
    );

    let db = state.db.background_pool.clone();
    tokio::spawn(async move {
        // The floor is what keeps a months-old backlog from flooding the recipient the first
        // time alerting is switched on; until it is recorded, nothing is eligible.
        while let Err(error) = thermite_core::alerts::ensure_backlog_floor(&db).await {
            tracing::error!(%error, "failed to record the alert backlog floor; retrying");
            tokio::time::sleep(std::time::Duration::from_secs(POLL_SECONDS)).await;
        }

        loop {
            match tick(&db, &sink).await {
                Ok(0) => {}
                Ok(count) => tracing::info!(count, "alerts delivered"),
                Err(error) => tracing::error!(%error, "alert tick failed"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(POLL_SECONDS)).await;
        }
    });
}

fn kind_label(alert: &Alert) -> &'static str {
    match alert.kind.as_str() {
        "regression" => "Regression",
        _ => "New issue",
    }
}

fn issue_url(alert: &Alert, base_url: &str) -> String {
    format!("{base_url}/issues/{}", alert.issue_id)
}

fn subject(alert: &Alert) -> String {
    format!(
        "[{}] {}: {}",
        alert.project_slug,
        kind_label(alert),
        alert.title
    )
}

/// Titles and culprits come from crash payloads — attacker-influenced text that must not become
/// markup in a mail client.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn email_body(alert: &Alert, base_url: &str) -> String {
    let mut facts = vec![
        escape(&alert.project_slug),
        escape(&alert.level),
        format!("{} events", alert.times_seen),
    ];
    if let Some(environment) = &alert.environment {
        facts.push(escape(environment));
    }
    if let Some(release) = &alert.release {
        facts.push(format!("release {}", escape(release)));
    }

    let culprit = alert
        .culprit
        .as_deref()
        .map(|culprit| format!("<p><code>{}</code></p>", escape(culprit)))
        .unwrap_or_default();

    format!(
        "<h2>{}: {}</h2>\
         <p>{}</p>\
         {}\
         <p><a href=\"{}\">Open in Thermite</a></p>",
        kind_label(alert),
        escape(&alert.title),
        facts.join(" · "),
        culprit,
        issue_url(alert, base_url),
    )
}

fn payload(alert: &Alert, base_url: &str) -> serde_json::Value {
    json!({
        "kind": alert.kind,
        "project": alert.project_slug,
        "issue_id": alert.issue_id,
        "title": alert.title,
        "culprit": alert.culprit,
        "level": alert.level,
        "times_seen": alert.times_seen,
        "environment": alert.environment,
        "release": alert.release,
        "url": issue_url(alert, base_url),
    })
}

#[cfg(test)]
#[path = "alerts_tests.rs"]
mod tests;

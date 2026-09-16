use crate::models::AppError;

/// Which login flow this instance runs. Inferred, never configured: `FERRISKEY_URL` decides it,
/// so there is no second variable that can disagree with the one that matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInMode {
    /// FerrisKey OIDC — what thermite.rs runs.
    FerrisKey,
    /// Thermite is its own identity provider: the env-configured admin password, an emailed
    /// code when SMTP is configured, and passkeys enrolled after the first login.
    Local,
}

/// FerrisKey connection details, present only in [`SignInMode::FerrisKey`].
#[derive(Debug, Clone)]
pub struct FerrisKeyConfig {
    pub url: String,
    /// Public OIDC issuer base URL. Unset: derived from `url`.
    pub issuer_url: Option<String>,
    pub realm: String,
    pub client_id: String,
}

/// SMTP transport settings, present only when `SMTP_HOST` is configured. The credentials live
/// in [`Secrets`]; these are the parts that are safe to print.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub from: String,
    pub security: smtp::SmtpSecurity,
}

/// Non-sensitive application configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub db_url: String,
    pub base_url: String,
    /// How people sign in. Derived from the rest of the configuration at load time.
    pub sign_in: SignInMode,
    /// `None` in local mode — there is no identity provider to reach.
    pub ferriskey: Option<FerrisKeyConfig>,
    pub secure_cookies: bool,
    pub trust_proxy_headers: bool,
    /// `None` when no `SMTP_HOST` is set: alerting then delivers to webhooks only, and local
    /// sign-in has no emailed code to offer.
    pub smtp: Option<SmtpConfig>,
    /// Address of the operator who may sign in with `THERMITE_ADMIN_PASSWORD`, lowercased.
    /// Local mode only; the hash itself is in [`Secrets`].
    pub admin_email: Option<String>,
    /// Comma-separated recipients for new-issue/regression alerts. Unset disables email alerts.
    pub alert_email: Option<String>,
    /// URL POSTed a JSON payload per alert. Unset disables webhook alerts.
    pub alert_webhook: Option<String>,
    /// Exact addresses allowed to self-register. Empty closes registration
    /// after the first account (see `crates/auth` `registration_allowed`).
    pub allowed_registration_emails: Vec<String>,
    /// Email domains allowed to self-register. Empty means no domain allowlist.
    pub allowed_registration_domains: Vec<String>,
    /// Slug of the one project anyone may read without signing in — its board and issues, with
    /// the DSN and alert routing withheld and nothing writable. Unset: no anonymous reads.
    pub demo_project: Option<String>,
    /// Whether `GET /demo` signs anyone in as the shared `demo` user. Projects are instance-wide,
    /// so this makes every visitor a writer on all of them: a public sandbox, never a real one.
    pub demo_autologin: bool,
    /// Where the landing page's "See the live demo" button goes. Unset: this instance's
    /// `demo_project` board, if there is one.
    pub demo_url: Option<String>,
    /// Whether the marketing site is served at all: the landing page, pricing and the legal
    /// pages. Off by default, so the published image is just the application — those pages are
    /// thermite.rs's, not a self-hoster's.
    pub site: bool,
    /// Hosted signup is behind a waitlist: the landing and pricing pages collect addresses instead
    /// of sending people to a registration that would refuse them.
    pub waitlist: bool,
    /// Umami website id. Unset: no tracker tag is written into the page at all — the image is
    /// public, so an instance nobody configured must phone nowhere.
    pub umami_website_id: Option<String>,
    /// Size of the interactive pool (dashboard, sessions, API, MCP).
    pub db_max_connections: u32,
    /// Size of the ingest pool — the ceiling on concurrent event digests.
    pub db_ingest_max_connections: u32,
}

impl Config {
    pub fn load_from_env() -> Result<Self, AppError> {
        let _ = dotenvy::dotenv();

        let admin_email = get_env_optional("THERMITE_ADMIN_EMAIL").map(|e| e.trim().to_lowercase());
        let mode = sign_in_mode(
            get_env_optional("FERRISKEY_URL").as_deref(),
            get_env_optional("SMTP_HOST").as_deref(),
            admin_email.as_deref(),
            get_env_optional("THERMITE_ADMIN_PASSWORD").as_deref(),
        )
        .map_err(AppError::Internal)?;

        let ferriskey = match mode {
            SignInMode::FerrisKey => Some(FerrisKeyConfig {
                url: get_env("FERRISKEY_URL")?,
                issuer_url: get_env_optional("FERRISKEY_ISSUER_URL"),
                realm: get_env("FERRISKEY_REALM")?,
                client_id: get_env("FERRISKEY_CLIENT_ID")?,
            }),
            SignInMode::Local => None,
        };

        let smtp = match get_env_optional("SMTP_HOST") {
            Some(host) => Some(SmtpConfig {
                port: get_env("SMTP_PORT")?
                    .parse()
                    .map_err(|_| AppError::Internal("Invalid SMTP_PORT".to_string()))?,
                from: get_env("SMTP_FROM")?,
                security: match get_env_optional("SMTP_SECURITY").as_deref() {
                    Some("tls") => smtp::SmtpSecurity::Tls,
                    Some("starttls") => smtp::SmtpSecurity::StartTls,
                    Some("none") => smtp::SmtpSecurity::None,
                    Some(other) => {
                        return Err(AppError::Internal(format!(
                            "Invalid SMTP_SECURITY: {other} (expected tls, starttls, or none)"
                        )));
                    }
                    None if is_local_smtp_host(&host) => smtp::SmtpSecurity::None,
                    None => smtp::SmtpSecurity::Tls,
                },
                host,
            }),
            None => None,
        };

        match mode {
            SignInMode::FerrisKey => tracing::info!("sign-in: FerrisKey OIDC"),
            SignInMode::Local => tracing::info!(
                admin = admin_email.is_some(),
                email_code = smtp.is_some(),
                "sign-in: local (no identity provider)"
            ),
        }

        Ok(Self {
            db_url: get_env("DATABASE_URL")?,
            base_url: get_env("BASE_URL")?,
            sign_in: mode,
            ferriskey,
            secure_cookies: get_env_optional("SECURE_COOKIES")
                .map(|v| v == "true")
                .unwrap_or(true),
            trust_proxy_headers: get_env_optional("TRUST_PROXY_HEADERS")
                .map(|v| v == "true")
                .unwrap_or(false),
            smtp,
            admin_email,
            alert_email: get_env_optional("THERMITE_ALERT_EMAIL"),
            alert_webhook: get_env_optional("THERMITE_ALERT_WEBHOOK"),
            allowed_registration_emails: parse_csv_lower(get_env_optional(
                "THERMITE_ALLOWED_EMAILS",
            )),
            allowed_registration_domains: parse_csv_lower(get_env_optional(
                "THERMITE_ALLOWED_EMAIL_DOMAINS",
            )),
            demo_project: get_env_optional("THERMITE_DEMO_PROJECT"),
            demo_autologin: get_env_optional("THERMITE_DEMO_AUTOLOGIN")
                .map(|v| v == "true")
                .unwrap_or(false),
            demo_url: get_env_optional("THERMITE_DEMO_URL"),
            site: get_env_optional("THERMITE_SITE")
                .map(|v| v == "true")
                .unwrap_or(false),
            waitlist: get_env_optional("THERMITE_WAITLIST")
                .map(|v| v == "true")
                .unwrap_or(false),
            umami_website_id: umami_website_id(get_env_optional("UMAMI_WEBSITE_ID")),
            db_max_connections: parse_env_or("DATABASE_MAX_CONNECTIONS", 10),
            db_ingest_max_connections: parse_env_or("DATABASE_INGEST_MAX_CONNECTIONS", 10),
        })
    }
}

/// Decide which login flow this deployment runs, or say exactly what is missing.
///
/// The rule is inference, not a switch: `FERRISKEY_URL` set means FerrisKey, unset means
/// thermite signs people in itself. A mode variable that could disagree with the credentials
/// actually configured would only ever be a way to boot into a login nobody can complete.
///
/// Local mode needs at least one way in — the admin credential, or SMTP for an emailed code.
/// FerrisKey mode needs SMTP either way: its OTP branch mails the code.
fn sign_in_mode(
    ferriskey_url: Option<&str>,
    smtp_host: Option<&str>,
    admin_email: Option<&str>,
    admin_password: Option<&str>,
) -> Result<SignInMode, String> {
    // The admin credential is a pair. Half of one is a typo, and silently ignoring it would
    // leave an operator staring at a login that refuses the password they think they set.
    match (admin_email, admin_password) {
        (Some(_), None) => {
            return Err(
                "THERMITE_ADMIN_EMAIL is set but THERMITE_ADMIN_PASSWORD is not: set both, or \
                 neither"
                    .to_string(),
            );
        }
        (None, Some(_)) => {
            return Err(
                "THERMITE_ADMIN_PASSWORD is set but THERMITE_ADMIN_EMAIL is not: set both, or \
                 neither"
                    .to_string(),
            );
        }
        _ => {}
    }

    if ferriskey_url.is_some() {
        if smtp_host.is_none() {
            return Err(
                "FERRISKEY_URL is set but SMTP_HOST is not: the FerrisKey login mails a \
                 verification code. Configure SMTP, or unset FERRISKEY_URL to sign in with \
                 THERMITE_ADMIN_EMAIL and THERMITE_ADMIN_PASSWORD instead."
                    .to_string(),
            );
        }
        return Ok(SignInMode::FerrisKey);
    }

    if admin_email.is_none() && smtp_host.is_none() {
        return Err(
            "no way to sign in is configured: set THERMITE_ADMIN_EMAIL and \
             THERMITE_ADMIN_PASSWORD, or SMTP_HOST (plus SMTP_PORT and SMTP_FROM) for \
             emailed sign-in codes, or FERRISKEY_URL to authenticate against FerrisKey."
                .to_string(),
        );
    }

    Ok(SignInMode::Local)
}

/// The Umami website id, or `None` for a value that is not one. It is interpolated into an
/// HTML attribute on every page the server renders, so anything outside the id's own alphabet
/// is refused rather than escaped — a typo in a deployment's environment must not be able to
/// close the attribute and inject markup.
fn umami_website_id(value: Option<String>) -> Option<String> {
    let id = value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(id);
    }
    tracing::warn!("ignoring UMAMI_WEBSITE_ID: expected ASCII letters, digits and '-' only");
    None
}

/// Parse a tuning variable, falling back to the default on a malformed value: a typo in a pool
/// size should not stop a deployment from starting.
fn parse_env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    let Some(raw) = get_env_optional(key) else {
        return default;
    };
    raw.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(key, value = %raw, "ignoring unparseable value, using the default");
        default
    })
}

/// Split a comma-separated env value into trimmed, lowercased, non-empty entries.
fn parse_csv_lower(value: Option<String>) -> Vec<String> {
    value
        .map(|v| {
            v.split(',')
                .map(|e| e.trim().to_lowercase())
                .filter(|e| !e.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Sensitive secrets loaded from environment variables.
///
/// Some fields are loaded but not yet read by the template binary itself —
/// they're placeholders ready to wire into downstream features.
#[derive(Clone)]
#[allow(dead_code)]
pub struct Secrets {
    pub session_secret: Vec<u8>,
    pub encryption_key: Option<[u8; 32]>,
    pub ferriskey_client_secret: Option<String>,
    pub smtp_user: secrecy::SecretString,
    pub smtp_password: secrecy::SecretString,
    /// Argon2 hash of `THERMITE_ADMIN_PASSWORD`, computed once at boot. The plaintext is never
    /// kept: a password in memory is a password in a core dump.
    pub admin_password_hash: Option<String>,
    /// A hash of a random string nobody has. `verify_password` runs against it when the address
    /// is not the admin's, so the time a rejection takes does not reveal which address that is.
    pub dummy_password_hash: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("session_secret", &"[REDACTED]")
            .finish()
    }
}

impl Secrets {
    pub fn load_from_env() -> Result<Self, AppError> {
        let _ = dotenvy::dotenv();

        let session_secret_hex = get_env("SESSION_SECRET")?;
        let session_secret = hex::decode(&session_secret_hex)
            .map_err(|e| AppError::Internal(format!("SESSION_SECRET must be valid hex: {e}")))?;

        if session_secret.len() < 64 {
            return Err(AppError::Internal(
                "SESSION_SECRET must be at least 64 bytes (128 hex chars)".to_string(),
            ));
        }

        let hash = |secret: &str| {
            crypto::hash_secret(secret)
                .map_err(|e| AppError::Internal(format!("Failed to hash a secret: {e}")))
        };
        let admin_password_hash = get_env_optional("THERMITE_ADMIN_PASSWORD")
            .map(|p| hash(&p))
            .transpose()?;
        let dummy_password_hash = hash(
            &crypto::generate_url_safe_token(32)
                .map_err(|e| AppError::Internal(format!("Failed to generate a token: {e}")))?,
        )?;

        let encryption_key = get_env_optional("ENCRYPTION_KEY")
            .map(|k| {
                crypto::encryption::parse_key(&k)
                    .map_err(|e| AppError::Internal(format!("Invalid ENCRYPTION_KEY: {e}")))
            })
            .transpose()?;

        Ok(Self {
            session_secret,
            encryption_key,
            ferriskey_client_secret: get_env_optional("FERRISKEY_CLIENT_SECRET"),
            smtp_user: secrecy::SecretString::from(
                get_env_optional("SMTP_USER").unwrap_or_default(),
            ),
            smtp_password: secrecy::SecretString::from(
                get_env_optional("SMTP_PASSWORD").unwrap_or_default(),
            ),
            admin_password_hash,
            dummy_password_hash,
        })
    }
}

fn get_env(key: &str) -> Result<String, AppError> {
    std::env::var(key).map_err(|_| AppError::Internal(format!("Missing required env var: {key}")))
}

fn get_env_optional(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn is_local_smtp_host(host: &str) -> bool {
    let h = host.to_lowercase();
    h == "localhost" || h == "mailpit" || h == "127.0.0.1" || h == "::1"
}

#[cfg(test)]
mod tests {
    use super::{SignInMode, sign_in_mode, umami_website_id};

    /// `FERRISKEY_URL` is the whole rule: set means FerrisKey, unset means thermite signs
    /// people in itself.
    #[test]
    fn the_identity_provider_decides_the_mode() {
        assert_eq!(
            sign_in_mode(Some("http://idp"), Some("smtp"), None, None),
            Ok(SignInMode::FerrisKey)
        );
        // An admin credential alongside FerrisKey changes nothing: production is unaffected.
        assert_eq!(
            sign_in_mode(
                Some("http://idp"),
                Some("smtp"),
                Some("a@b.test"),
                Some("pw")
            ),
            Ok(SignInMode::FerrisKey)
        );
        assert_eq!(
            sign_in_mode(None, None, Some("a@b.test"), Some("pw")),
            Ok(SignInMode::Local)
        );
        // SMTP alone is enough: the emailed code is a way in on its own.
        assert_eq!(
            sign_in_mode(None, Some("smtp"), None, None),
            Ok(SignInMode::Local)
        );
    }

    #[test]
    fn a_deployment_nobody_could_log_into_refuses_to_boot() {
        let err = sign_in_mode(None, None, None, None).unwrap_err();
        assert!(err.contains("THERMITE_ADMIN_EMAIL"), "{err}");
        assert!(err.contains("SMTP_HOST"), "{err}");
        assert!(err.contains("FERRISKEY_URL"), "{err}");
    }

    /// Half an admin credential is a typo, and ignoring it leaves an operator staring at a
    /// login that refuses the password they think they set.
    #[test]
    fn half_an_admin_credential_names_the_missing_half() {
        let err = sign_in_mode(None, Some("smtp"), Some("a@b.test"), None).unwrap_err();
        assert!(err.contains("THERMITE_ADMIN_PASSWORD is not"), "{err}");

        let err = sign_in_mode(None, Some("smtp"), None, Some("pw")).unwrap_err();
        assert!(err.contains("THERMITE_ADMIN_EMAIL is not"), "{err}");
    }

    /// FerrisKey's own login mails a verification code, so it cannot run without SMTP —
    /// unchanged from before local mode existed.
    #[test]
    fn ferriskey_still_requires_smtp() {
        let err = sign_in_mode(Some("http://idp"), None, Some("a@b.test"), Some("pw")).unwrap_err();
        assert!(err.contains("SMTP_HOST"), "{err}");
    }

    #[test]
    fn a_website_id_that_could_carry_markup_is_refused() {
        let id = "af413d12-39d7-4060-bc8d-6856f5b74ae1";
        assert_eq!(
            umami_website_id(Some(format!("  {id}  "))).as_deref(),
            Some(id)
        );
        assert_eq!(umami_website_id(Some(String::new())), None);
        assert_eq!(umami_website_id(None), None);
        // The value lands inside an HTML attribute; a quote must never reach it.
        assert_eq!(
            umami_website_id(Some(r#"x"><script>alert(1)</script>"#.to_string())),
            None
        );
    }
}

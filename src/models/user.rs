use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use uuid::Uuid;

/// User entity stored in PostgreSQL.
#[cfg(feature = "server")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserEntity {
    pub id: Uuid,

    /// OIDC subject identifier (from FerrisKey)
    pub sub: String,

    /// User email address
    pub email: String,

    /// Display name
    pub name: Option<String>,

    /// Avatar URL
    pub avatar_url: Option<String>,

    /// Current subscription state. No producer since the Polar integration was
    /// removed; the column is retained so it keeps round-tripping.
    #[serde(default)]
    pub subscription: Option<crate::models::subscription::SubscriptionInfo>,

    /// The terms version the user accepted, and when. None until they do; an older version
    /// than dx-auth's current one makes it ask again.
    #[serde(default)]
    pub tos_version: Option<String>,
    #[serde(default)]
    pub tos_accepted_at: Option<chrono::DateTime<chrono::Utc>>,

    /// When the user was created
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// When the user was last updated
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Logged-in user data available on both client and server.
/// Mirrors `auth::LoggedInData` but is available on both compilation targets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedInData {
    pub id: String,
    pub sub: String,
    pub email: String,
    pub username: String,
    pub avatar_url: Option<String>,
}

#[cfg(feature = "server")]
impl From<auth::LoggedInData> for LoggedInData {
    fn from(data: auth::LoggedInData) -> Self {
        Self {
            id: data.id,
            sub: data.sub,
            email: data.email,
            username: data.username,
            avatar_url: data.avatar_url,
        }
    }
}

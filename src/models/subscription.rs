//! Subscription state embedded on the user.
//!
//! Retained without a producer: the Polar integration was removed because it was
//! never configured in any environment, but `users.subscription` is a live JSONB
//! column with a migration behind it. This type is what keeps that column
//! round-tripping. Nothing writes it today — wiring billing back up means adding
//! a writer, not a schema change.

use serde::{Deserialize, Serialize};

/// A user's current subscription, as last reported by Polar.
///
/// Only server code constructs it (JSONB reads and their tests); the client
/// merely carries it through `UserInfo` deserialization.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionInfo {
    pub subscription_id: String,
    pub customer_id: String,
    /// Raw Polar status, e.g. "active", "trialing", "canceled", "past_due".
    pub status: String,
    /// Tier label from the product metadata, if present.
    pub tier: Option<String>,
    /// End of the current period (unix milliseconds), if known.
    pub current_period_end: Option<i64>,
    /// When we last applied a webhook for this subscription (RFC 3339).
    pub updated_at: String,
}

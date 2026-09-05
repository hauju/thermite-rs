//! Context that outlives one event.
//!
//! One scope per client, not a stack of them. The Sentry SDKs keep a hub with a scope stack so a
//! request handler can push context that unwinds with it; that machinery is most of their size,
//! and it answers a question a process reporting its own errors does not ask.
//!
//! Everything here is applied only where the event did not already say. An event that carries its
//! own `user` was built by a caller who knew better than the ambient scope.

use std::collections::{BTreeMap, VecDeque};

use crate::event::{Breadcrumb, Event, User, Values};

/// Breadcrumbs kept, and therefore sent on every event.
///
/// Sentry's default is 100. This is lower because they ride on *every* event: at roughly 100 bytes
/// each, 100 breadcrumbs is 10 KB per report, and the browser transport has to fit a whole envelope
/// inside a 64 KiB keepalive body.
pub const DEFAULT_MAX_BREADCRUMBS: usize = 30;

/// Tags, user and breadcrumbs, stamped onto every event the client sends.
#[derive(Debug)]
pub struct Scope {
    /// Indexed by thermite into `issue_tags`, so these are filterable. Values are
    /// client-controlled and the rollup caps distinct values per issue — a tag whose value is
    /// unique per event buys nothing and spends that cap.
    pub tags: BTreeMap<String, String>,
    pub user: Option<User>,
    breadcrumbs: VecDeque<Breadcrumb>,
    max_breadcrumbs: usize,
}

impl Scope {
    pub fn new(max_breadcrumbs: usize) -> Self {
        Self {
            tags: BTreeMap::new(),
            user: None,
            breadcrumbs: VecDeque::new(),
            max_breadcrumbs,
        }
    }

    /// Records a breadcrumb, dropping the oldest once the buffer is full.
    ///
    /// A no-op at zero capacity, which is how breadcrumbs are switched off.
    pub fn add_breadcrumb(&mut self, breadcrumb: Breadcrumb) {
        if self.max_breadcrumbs == 0 {
            return;
        }

        self.breadcrumbs.push_back(breadcrumb);
        while self.breadcrumbs.len() > self.max_breadcrumbs {
            self.breadcrumbs.pop_front();
        }
    }

    /// The breadcrumbs currently buffered, oldest first.
    pub fn breadcrumbs(&self) -> impl Iterator<Item = &Breadcrumb> {
        self.breadcrumbs.iter()
    }

    /// Fills in what the event left blank.
    pub(crate) fn apply(&self, event: &mut Event) {
        for (key, value) in &self.tags {
            event
                .tags
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        // An empty `User` would synthesize no `user` tag and read as a blank row on the issue
        // page, so it is treated as absent rather than sent.
        if event.user.is_none() && self.user.as_ref().is_some_and(|user| !user.is_empty()) {
            event.user.clone_from(&self.user);
        }

        if event.breadcrumbs.is_none() && !self.breadcrumbs.is_empty() {
            event.breadcrumbs = Some(Values {
                values: self.breadcrumbs.iter().cloned().collect(),
            });
        }
    }
}

impl Default for Scope {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BREADCRUMBS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Level;

    fn messages(event: &Event) -> Vec<String> {
        event
            .breadcrumbs
            .as_ref()
            .map(|values| {
                values
                    .values
                    .iter()
                    .filter_map(|crumb| crumb.message.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn breadcrumbs_drop_the_oldest_once_the_buffer_is_full() {
        let mut scope = Scope::new(2);
        for message in ["first", "second", "third"] {
            scope.add_breadcrumb(Breadcrumb::new(message));
        }

        let mut event = Event::message("boom", Level::Error);
        scope.apply(&mut event);

        assert_eq!(messages(&event), vec!["second", "third"]);
    }

    #[test]
    fn a_zero_capacity_scope_records_nothing() {
        let mut scope = Scope::new(0);
        scope.add_breadcrumb(Breadcrumb::new("first"));

        let mut event = Event::message("boom", Level::Error);
        scope.apply(&mut event);

        assert!(event.breadcrumbs.is_none());
    }

    #[test]
    fn tags_and_user_are_stamped_onto_an_event_that_has_none() {
        let mut scope = Scope::default();
        scope.tags.insert("component".into(), "billing".into());
        scope.user = Some(User {
            id: Some("u-1".into()),
            ..User::default()
        });

        let mut event = Event::message("boom", Level::Error);
        scope.apply(&mut event);

        assert_eq!(event.tags["component"], "billing");
        assert_eq!(event.user.unwrap().id.unwrap(), "u-1");
    }

    /// The event wins. A caller that set a tag knew something the ambient scope did not.
    #[test]
    fn an_event_keeps_the_tags_and_user_it_set_itself() {
        let mut scope = Scope::default();
        scope.tags.insert("component".into(), "billing".into());
        scope.user = Some(User {
            id: Some("ambient".into()),
            ..User::default()
        });

        let mut event = Event::message("boom", Level::Error);
        event.tags.insert("component".into(), "worker".into());
        event.user = Some(User {
            id: Some("specific".into()),
            ..User::default()
        });
        scope.apply(&mut event);

        assert_eq!(event.tags["component"], "worker");
        assert_eq!(event.user.unwrap().id.unwrap(), "specific");
    }

    /// A `User` with nothing in it yields no `user_key`, so sending it would add a blank row to
    /// the issue page and nothing else.
    #[test]
    fn an_empty_user_is_not_stamped_on() {
        let mut scope = Scope::new(DEFAULT_MAX_BREADCRUMBS);
        scope.user = Some(User::default());

        let mut event = Event::message("boom", Level::Error);
        scope.apply(&mut event);

        assert!(event.user.is_none());
    }
}

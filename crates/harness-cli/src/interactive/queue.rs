//! What the user queued while the agent worked: prime-agent's two lanes.
//!
//! prime-agent keeps a **steering** lane - messages the running turn should read
//! at its next step - and a **follow-up** lane - messages that start a turn of
//! their own after the running one ends. Each lane is drained `all` at once (every
//! queued message in one turn) or `one-at-a-time` (the default). Here Enter while
//! the agent works steers the running turn directly; a steering message lands in
//! this queue only when the turn cannot take it yet. `/queue <text>` (alias
//! `/followup`) adds a follow-up. The queue survives Ctrl+C: what the user queued
//! is still theirs to send.

/// Which lane a queued message waits in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    Steer,
    FollowUp,
}

impl Lane {
    /// prime-agent's label for the lane.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Steer => "Steering",
            Self::FollowUp => "Follow-up",
        }
    }
}

/// How a lane is drained (prime-agent's `steeringMode` / `followUpMode`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QueueMode {
    /// Every queued message of the lane in one turn.
    All,
    /// One message per turn.
    #[default]
    OneAtATime,
}

impl QueueMode {
    /// `all` or `one-at-a-time`, as prime-agent spells them.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "all" => Some(Self::All),
            "one-at-a-time" => Some(Self::OneAtATime),
            _ => None,
        }
    }
}

/// The queued messages, oldest first.
#[derive(Clone, Debug, Default)]
pub struct InputQueue {
    items: Vec<(Lane, String)>,
    steering_mode: QueueMode,
    follow_up_mode: QueueMode,
}

impl InputQueue {
    #[must_use]
    pub fn new(steering_mode: QueueMode, follow_up_mode: QueueMode) -> Self {
        Self {
            items: Vec::new(),
            steering_mode,
            follow_up_mode,
        }
    }

    pub fn push(&mut self, lane: Lane, text: String) {
        self.items.push((lane, text));
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The next turn's text from `lane`: its oldest message, or all of them
    /// joined when the lane is drained all at once.
    pub fn take_next(&mut self, lane: Lane) -> Option<String> {
        let mode = match lane {
            Lane::Steer => self.steering_mode,
            Lane::FollowUp => self.follow_up_mode,
        };
        let first = self.items.iter().position(|(each, _)| *each == lane)?;
        if mode == QueueMode::OneAtATime {
            return Some(self.items.remove(first).1);
        }
        let mut taken = Vec::new();
        self.items.retain(|(each, text)| {
            if *each == lane {
                taken.push(text.clone());
                false
            } else {
                true
            }
        });
        Some(taken.join("\n\n"))
    }

    /// prime-agent's preview rows (`formatQueuedMessagePreview`): the lane
    /// label and the message's first line, oldest first.
    #[must_use]
    pub fn previews(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|(lane, text)| {
                let first = text.lines().next().unwrap_or_default();
                format!("{}: {first}", lane.label())
            })
            .collect()
    }

    /// Remove every message `drop` picks; the removed ones, oldest first.
    pub fn remove_where(&mut self, drop: impl Fn(&str) -> bool) -> Vec<(Lane, String)> {
        let mut removed = Vec::new();
        self.items.retain(|(lane, text)| {
            if drop(text) {
                removed.push((*lane, text.clone()));
                false
            } else {
                true
            }
        });
        removed
    }

    /// Remove the message at `index` (0-based, oldest first).
    pub fn remove(&mut self, index: usize) -> Option<(Lane, String)> {
        (index < self.items.len()).then(|| self.items.remove(index))
    }

    /// Replace the text at `index`; an empty text removes the message.
    pub fn edit(&mut self, index: usize, text: String) -> bool {
        if index >= self.items.len() {
            return false;
        }
        if text.trim().is_empty() {
            self.items.remove(index);
        } else {
            self.items[index].1 = text;
        }
        true
    }

    /// Move the message at `index` one place earlier.
    pub fn move_up(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.items.len() {
            return false;
        }
        self.items.swap(index, index - 1);
        true
    }

    /// Move the message at `index` one place later.
    pub fn move_down(&mut self, index: usize) -> bool {
        if index + 1 >= self.items.len() {
            return false;
        }
        self.items.swap(index, index + 1);
        true
    }

    #[cfg(test)]
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&(Lane, String)> {
        self.items.get(index)
    }

    /// One line per message, numbered from 1: `1. Follow-up: text`.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.items
            .iter()
            .enumerate()
            .map(|(index, (lane, text))| {
                let first = text.lines().next().unwrap_or_default();
                format!("{}. {}: {first}", index + 1, lane.label())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{InputQueue, Lane, QueueMode};

    #[test]
    fn q05_queue_take_next_respects_the_mode() {
        let mut queue = InputQueue::new(QueueMode::OneAtATime, QueueMode::OneAtATime);
        queue.push(Lane::FollowUp, "summarize".to_owned());
        queue.push(Lane::Steer, "also tests".to_owned());
        queue.push(Lane::FollowUp, "translate".to_owned());
        assert_eq!(queue.take_next(Lane::Steer).as_deref(), Some("also tests"));
        assert_eq!(queue.take_next(Lane::Steer), None);
        assert_eq!(
            queue.take_next(Lane::FollowUp).as_deref(),
            Some("summarize")
        );
        assert_eq!(
            queue.take_next(Lane::FollowUp).as_deref(),
            Some("translate")
        );
        assert!(queue.is_empty());

        let mut all = InputQueue::new(QueueMode::OneAtATime, QueueMode::All);
        all.push(Lane::FollowUp, "one".to_owned());
        all.push(Lane::Steer, "keep".to_owned());
        all.push(Lane::FollowUp, "two".to_owned());
        assert_eq!(all.take_next(Lane::FollowUp).as_deref(), Some("one\n\ntwo"));
        assert_eq!(all.len(), 1, "the other lane stays");
    }

    #[test]
    fn q06_edit_remove_and_reorder() {
        let mut queue = InputQueue::default();
        for text in ["a", "b", "c"] {
            queue.push(Lane::FollowUp, text.to_owned());
        }
        assert!(queue.move_up(2));
        assert_eq!(
            queue.lines(),
            ["1. Follow-up: a", "2. Follow-up: c", "3. Follow-up: b"]
        );
        assert!(queue.move_down(0));
        assert!(!queue.move_up(0), "the first cannot move earlier");
        assert!(queue.edit(0, "C!".to_owned()));
        assert_eq!(queue.get(0).map(|(_, text)| text.as_str()), Some("C!"));
        assert!(queue.edit(0, "  ".to_owned()), "an empty edit removes");
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.remove(5), None);
        assert_eq!(queue.remove(0).map(|(_, text)| text), Some("a".to_owned()));
        assert_eq!(QueueMode::parse("all"), Some(QueueMode::All));
        assert_eq!(
            QueueMode::parse("one-at-a-time"),
            Some(QueueMode::OneAtATime)
        );
        assert_eq!(QueueMode::parse("some"), None);
    }
}

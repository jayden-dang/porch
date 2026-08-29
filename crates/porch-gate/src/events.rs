//! Bounded per-subscriber event mailbox for daemon IPC / TUI.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Per-subscriber mailbox capacity.
pub const MAILBOX_CAP: usize = 64;

/// Events streamed on `subscribe` after the JSON-RPC ack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Client must call `get_run` (or `list_runs`) to refresh.
    StreamGap { state_rev: u64 },
    /// Compact hint that run state changed; snapshot is `get_run`.
    State { state_rev: u64, run_id: String },
    /// Droppable log-ish line.
    Activity { run_id: String, text: String },
}

struct Mailbox {
    queue: VecDeque<Event>,
    /// When true, next read yields [`Event::StreamGap`] with `latest_rev`.
    sticky_gap: bool,
    latest_rev: u64,
    filter_run_id: Option<String>,
}

struct HubInner {
    state_rev: AtomicU64,
    next_id: AtomicU64,
    subs: Mutex<HashMap<u64, Mailbox>>,
    cv: Condvar,
}

/// Process-wide hub installed by the daemon; publish is a no-op when unset.
static INSTALLED: Mutex<Option<Arc<EventHub>>> = Mutex::new(None);

/// Shared event fan-out. Publishers never block on subscribers.
#[derive(Clone)]
pub struct EventHub {
    inner: Arc<HubInner>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EventHub {
    /// Create an empty hub with `state_rev` starting at 0.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HubInner {
                state_rev: AtomicU64::new(0),
                next_id: AtomicU64::new(1),
                subs: Mutex::new(HashMap::new()),
                cv: Condvar::new(),
            }),
        }
    }

    /// Monotonic revision; advanced on every [`Self::publish_state`].
    #[must_use]
    pub fn state_rev(&self) -> u64 {
        self.inner.state_rev.load(Ordering::SeqCst)
    }

    /// Subscribe; optional `run_id` filters state/activity to that run.
    ///
    /// The first event is always a [`Event::StreamGap`] so the client must
    /// refresh via `get_run`.
    ///
    /// # Panics
    ///
    /// Panics if the subscriber map mutex is poisoned.
    #[must_use]
    pub fn subscribe(&self, run_id: Option<&str>) -> Subscriber {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let rev = self.state_rev();
        let mailbox = Mailbox {
            queue: VecDeque::from([Event::StreamGap { state_rev: rev }]),
            sticky_gap: false,
            latest_rev: rev,
            filter_run_id: run_id.map(str::to_string),
        };
        self.inner
            .subs
            .lock()
            .expect("event hub")
            .insert(id, mailbox);
        self.inner.cv.notify_all();
        Subscriber {
            id,
            hub: Arc::clone(&self.inner),
        }
    }

    /// Publish a state change. Never blocks. Advances `state_rev`.
    ///
    /// # Panics
    ///
    /// Panics if the subscriber map mutex is poisoned.
    pub fn publish_state(&self, run_id: &str) {
        let rev = self.inner.state_rev.fetch_add(1, Ordering::SeqCst) + 1;
        let mut guard = self.inner.subs.lock().expect("event hub");
        for mb in guard.values_mut() {
            if let Some(filter) = &mb.filter_run_id {
                if filter != run_id {
                    continue;
                }
            }
            mb.latest_rev = rev;
            let ev = Event::State {
                state_rev: rev,
                run_id: run_id.to_string(),
            };
            if mb.queue.len() < MAILBOX_CAP {
                mb.queue.push_back(ev);
            } else {
                // Must not silently lose state: sticky gap forces get_run.
                mb.sticky_gap = true;
            }
        }
        drop(guard);
        self.inner.cv.notify_all();
    }

    /// Publish activity. Never blocks. May drop under pressure (newest first).
    ///
    /// # Panics
    ///
    /// Panics if the subscriber map mutex is poisoned.
    pub fn publish_activity(&self, run_id: &str, text: &str) {
        let mut guard = self.inner.subs.lock().expect("event hub");
        for mb in guard.values_mut() {
            if let Some(filter) = &mb.filter_run_id {
                if filter != run_id {
                    continue;
                }
            }
            if mb.queue.len() >= MAILBOX_CAP {
                // Drop incoming so the unread prefix stays.
                continue;
            }
            mb.queue.push_back(Event::Activity {
                run_id: run_id.to_string(),
                text: text.to_string(),
            });
        }
        drop(guard);
        self.inner.cv.notify_all();
    }
}

/// Install `hub` as the process-wide publisher target for `porch-run`.
///
/// # Panics
///
/// Panics if the install mutex is poisoned.
pub fn install_event_hub(hub: Arc<EventHub>) {
    *INSTALLED.lock().expect("event hub install") = Some(hub);
}

/// Clear the process-wide hub (tests / shutdown).
///
/// # Panics
///
/// Panics if the install mutex is poisoned.
pub fn clear_event_hub() {
    *INSTALLED.lock().expect("event hub install") = None;
}

/// Process-wide hub, if the daemon installed one.
///
/// # Panics
///
/// Panics if the install mutex is poisoned.
#[must_use]
pub fn event_hub() -> Option<Arc<EventHub>> {
    INSTALLED.lock().expect("event hub install").clone()
}

/// Live subscription; unregisters on drop.
pub struct Subscriber {
    pub(crate) id: u64,
    hub: Arc<HubInner>,
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.hub.subs.lock().expect("event hub").remove(&self.id);
    }
}

impl Subscriber {
    /// Non-blocking poll; returns `None` if nothing is ready.
    ///
    /// # Panics
    ///
    /// Panics if the subscriber map mutex is poisoned.
    #[must_use]
    pub fn try_recv(&self) -> Option<Event> {
        let mut guard = self.hub.subs.lock().expect("event hub");
        pop_event(&mut guard, self.id)
    }

    /// Block until an event is available or `timeout` elapses.
    ///
    /// # Panics
    ///
    /// Panics if the subscriber map mutex is poisoned.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        let guard = self.hub.subs.lock().expect("event hub");
        let (mut guard, _) = self
            .hub
            .cv
            .wait_timeout_while(guard, timeout, |g| {
                g.get(&self.id)
                    .is_some_and(|mb| !mb.sticky_gap && mb.queue.is_empty())
            })
            .expect("event hub");
        pop_event(&mut guard, self.id)
    }
}

fn pop_event(guard: &mut MutexGuard<'_, HashMap<u64, Mailbox>>, id: u64) -> Option<Event> {
    let mb = guard.get_mut(&id)?;
    if mb.sticky_gap {
        mb.sticky_gap = false;
        // Drop queued items that are now stale relative to the gap.
        mb.queue.clear();
        return Some(Event::StreamGap {
            state_rev: mb.latest_rev,
        });
    }
    mb.queue.pop_front()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn new_subscriber_first_event_is_stream_gap() {
        let hub = EventHub::new();
        let sub = hub.subscribe(None);
        let ev = sub.try_recv().expect("gap");
        assert!(matches!(ev, Event::StreamGap { state_rev: 0 }));
        assert!(sub.try_recv().is_none());
    }

    #[test]
    fn activity_overflow_does_not_block_and_drops_newest() {
        let hub = EventHub::new();
        let sub = hub.subscribe(None);
        let _ = sub.try_recv(); // drain initial gap

        for i in 0..(MAILBOX_CAP + 10) {
            hub.publish_activity("run-a", &format!("line-{i}"));
        }
        // Publisher returned; mailbox holds at most CAP events.
        let mut count = 0;
        let mut texts = Vec::new();
        while let Some(ev) = sub.try_recv() {
            count += 1;
            if let Event::Activity { text, .. } = ev {
                texts.push(text);
            }
        }
        assert!(count <= MAILBOX_CAP);
        assert_eq!(texts.first().map(String::as_str), Some("line-0"));
        assert!(
            !texts
                .iter()
                .any(|t| t == &format!("line-{}", MAILBOX_CAP + 9))
        );
    }

    #[test]
    fn state_overflow_sets_sticky_gap_with_latest_rev() {
        let hub = EventHub::new();
        let sub = hub.subscribe(None);
        let _ = sub.try_recv();

        // Fill with activity so state cannot enqueue.
        for i in 0..MAILBOX_CAP {
            hub.publish_activity("run-a", &format!("a{i}"));
        }
        hub.publish_state("run-a");
        let rev = hub.state_rev();
        assert_eq!(rev, 1);

        // Drain activities then expect gap (sticky), not a silent drop.
        let mut saw_gap = None;
        for _ in 0..(MAILBOX_CAP + 2) {
            match sub.try_recv() {
                Some(Event::StreamGap { state_rev }) => {
                    saw_gap = Some(state_rev);
                    break;
                }
                Some(Event::Activity { .. }) => {}
                Some(Event::State { .. }) => panic!("state should not fit when full"),
                None => break,
            }
        }
        // If activities filled the queue, sticky gap surfaces on next read after
        // drain, or immediately if pop cleared via sticky before activities.
        if saw_gap.is_none() {
            // Force another state with full queue of remaining events, or empty.
            for i in 0..MAILBOX_CAP {
                hub.publish_activity("run-a", &format!("b{i}"));
            }
            hub.publish_state("run-a");
            // Drain until gap
            for _ in 0..(MAILBOX_CAP + 5) {
                match sub.try_recv() {
                    Some(Event::StreamGap { state_rev }) => {
                        saw_gap = Some(state_rev);
                        break;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        }
        assert_eq!(saw_gap, Some(hub.state_rev()));
    }

    #[test]
    fn slow_subscriber_does_not_block_fast_subscriber() {
        let hub = EventHub::new();
        let slow = hub.subscribe(None);
        let fast = hub.subscribe(None);
        let _ = slow.try_recv();
        let _ = fast.try_recv();

        hub.publish_state("r1");
        // Fast receives immediately even if slow never reads.
        let ev = fast
            .recv_timeout(Duration::from_millis(200))
            .expect("fast got state");
        assert!(matches!(ev, Event::State { run_id, .. } if run_id == "r1"));
        // Slow still has it queued.
        assert!(matches!(
            slow.try_recv(),
            Some(Event::State { run_id, .. }) if run_id == "r1"
        ));
    }

    #[test]
    fn drop_unregisters_subscriber() {
        let hub = EventHub::new();
        let sub = hub.subscribe(None);
        let id = sub.id;
        drop(sub);
        assert!(!hub.inner.subs.lock().unwrap().contains_key(&id));
    }

    #[test]
    fn install_and_clear_global_hub() {
        clear_event_hub();
        assert!(event_hub().is_none());
        let hub = Arc::new(EventHub::new());
        install_event_hub(Arc::clone(&hub));
        assert!(event_hub().is_some());
        event_hub().unwrap().publish_activity("x", "hi");
        clear_event_hub();
        assert!(event_hub().is_none());
    }

    #[test]
    fn publish_does_not_block_when_subscriber_asleep() {
        let hub = Arc::new(EventHub::new());
        let sub = hub.subscribe(None);
        let _ = sub.try_recv();
        let hub2 = Arc::clone(&hub);
        let handle = thread::spawn(move || {
            for i in 0..200 {
                hub2.publish_state(&format!("r{i}"));
                hub2.publish_activity("r0", "x");
            }
        });
        // Do not read from sub while publisher runs.
        handle.join().unwrap();
        // Either state events or a sticky gap must be observable.
        let mut saw = false;
        for _ in 0..MAILBOX_CAP + 5 {
            match sub.try_recv() {
                Some(Event::State { .. } | Event::StreamGap { .. }) => {
                    saw = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw);
    }
}

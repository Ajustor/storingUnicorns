//! Saving app state off the UI thread: writes are coalesced (at most one
//! pending, the last state wins) and consoles are saved once their text has
//! stopped changing for a moment.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Consoles are saved this long after their last edit.
pub const CONSOLES_DELAY: Duration = Duration::from_secs(2);

/// Whether state edited at `last_edit` should be saved at `now`: it has not
/// changed for `delay`.
pub fn should_save(last_edit: Option<Instant>, now: Instant, delay: Duration) -> bool {
    last_edit.is_some_and(|t| now.saturating_duration_since(t) >= delay)
}

/// Time left before a save is due (`None`: nothing to save).
pub fn save_due_in(last_edit: Option<Instant>, now: Instant, delay: Duration) -> Option<Duration> {
    last_edit.map(|t| delay.saturating_sub(now.saturating_duration_since(t)))
}

struct State<T> {
    /// Value waiting to be written (a newer one replaces it).
    pending: Option<T>,
    /// A writer thread is running.
    busy: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    idle: Condvar,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Writes values with `write` on a background thread, one at a time. A value
/// queued while another is being written waits; a newer one replaces it.
pub struct Saver<T: Send + 'static> {
    shared: Arc<Shared<T>>,
    write: Arc<dyn Fn(&T) + Send + Sync>,
}

impl<T: Send + 'static> Saver<T> {
    pub fn new(write: impl Fn(&T) + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    pending: None,
                    busy: false,
                }),
                idle: Condvar::new(),
            }),
            write: Arc::new(write),
        }
    }

    /// Queue `value` for writing; never blocks on the write itself.
    pub fn save(&self, value: T) {
        let mut state = self.shared.lock();
        state.pending = Some(value);
        if state.busy {
            return;
        }
        state.busy = true;
        drop(state);
        let (shared, write) = (self.shared.clone(), self.write.clone());
        std::thread::spawn(move || loop {
            let next = {
                let mut state = shared.lock();
                match state.pending.take() {
                    Some(v) => v,
                    None => {
                        state.busy = false;
                        shared.idle.notify_all();
                        return;
                    }
                }
            };
            write(&next);
        });
    }

    /// Wait until every queued value has been written.
    pub fn flush(&self) {
        let mut state = self.shared.lock();
        while state.busy {
            state = self
                .shared
                .idle
                .wait(state)
                .unwrap_or_else(|p| p.into_inner());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn save_is_due_after_the_delay_since_the_last_edit() {
        let t0 = Instant::now();
        let d = Duration::from_secs(2);
        assert!(!should_save(None, t0, d), "nothing edited");
        assert!(!should_save(Some(t0), t0 + Duration::from_millis(1999), d));
        assert!(should_save(Some(t0), t0 + d, d));
        assert!(should_save(Some(t0), t0 + Duration::from_secs(9), d));
        assert!(!should_save(Some(t0 + d), t0, d), "clock before the edit");
        assert_eq!(save_due_in(None, t0, d), None);
        assert_eq!(
            save_due_in(Some(t0), t0 + Duration::from_millis(500), d),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(save_due_in(Some(t0), t0 + d * 3, d), Some(Duration::ZERO));
    }

    #[test]
    fn writes_coalesce_and_the_last_state_wins() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let w = written.clone();
        let saver = Saver::new(move |v: &u32| {
            let _ = started_tx.send(*v);
            if *v == 1 {
                // Hold the first write until the test queued the others.
                release_rx.lock().unwrap().recv().unwrap();
            }
            w.lock().unwrap().push(*v);
        });
        saver.save(1);
        assert_eq!(started.recv().unwrap(), 1, "the first write started");
        saver.save(2);
        saver.save(3);
        release.send(()).unwrap();
        saver.flush();
        assert_eq!(*written.lock().unwrap(), vec![1, 3], "2 was superseded");

        saver.save(4);
        saver.flush();
        assert_eq!(
            written.lock().unwrap().last(),
            Some(&4),
            "writes again later"
        );
        saver.flush(); // idle: returns at once
    }
}

use crate::{files::SavedCapture, storage::Storage};
use std::io;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

pub(crate) const TIMER: usize = 1;

pub(crate) enum Event<'a> {
    Request,
    Saved(&'a SavedCapture),
    ScanFinished(Option<Instant>),
    RetentionChanged(u32),
}

pub(crate) struct Cleanup {
    retention: Option<u32>,
    scheduled: Option<Instant>,
    pending: Option<PendingScan>,
}

struct PendingScan {
    current: Arc<AtomicBool>,
    repeat: bool,
}

pub(crate) struct Scan {
    minutes: u32,
    current: Arc<AtomicBool>,
}

impl Scan {
    pub fn run(self, storage: &Storage, running: impl Fn() -> bool) -> Option<Instant> {
        let current = || running() && self.current.load(Ordering::Acquire);
        if !current() {
            return None;
        }
        let started = Instant::now();
        let delay = storage.clean(self.minutes, current)?;
        started.checked_add(Duration::from_millis(delay))
    }
}

impl Cleanup {
    pub fn new(retention: Option<u32>) -> Self {
        Self {
            retention,
            scheduled: None,
            pending: None,
        }
    }

    pub fn handle(
        &mut self,
        event: Event<'_>,
        hwnd: HWND,
        enqueue: impl Fn(Scan) -> io::Result<()>,
    ) -> io::Result<()> {
        self.update(event, &WindowsCleanup { hwnd, enqueue })
    }

    fn update(&mut self, event: Event<'_>, effects: &impl Effects) -> io::Result<()> {
        match event {
            Event::Request => self.request(effects),
            Event::Saved(saved) => {
                // Saves and scans share the serial worker. A saved outcome received during
                // a pending scan will already be covered by that scan.
                if let Some(minutes) = self.retention.filter(|_| self.pending.is_none()) {
                    if let Some(deadline) = effects.now().checked_add(saved.expires_in(minutes)) {
                        self.schedule(deadline, effects)?;
                    }
                }
                Ok(())
            }
            Event::ScanFinished(deadline) => {
                let Some(pending) = self.pending.take() else {
                    return Ok(());
                };
                if pending.repeat {
                    self.request(effects)
                } else if let Some(deadline) = deadline {
                    self.schedule(deadline, effects)
                } else {
                    Ok(())
                }
            }
            Event::RetentionChanged(minutes) => {
                if self.retention.replace(minutes) != Some(minutes) {
                    if let Some(pending) = &self.pending {
                        pending.current.store(false, Ordering::Release);
                    }
                }
                self.request(effects)
            }
        }
    }

    fn request(&mut self, effects: &impl Effects) -> io::Result<()> {
        effects.clear_timer();
        self.scheduled = None;
        let Some(minutes) = self.retention else {
            return Ok(());
        };
        if let Some(pending) = &mut self.pending {
            pending.repeat = true;
        } else {
            let current = Arc::new(AtomicBool::new(true));
            effects.start_scan(Scan {
                minutes,
                current: Arc::clone(&current),
            })?;
            self.pending = Some(PendingScan {
                current,
                repeat: false,
            });
        }
        Ok(())
    }

    fn schedule(&mut self, deadline: Instant, effects: &impl Effects) -> io::Result<()> {
        if self.scheduled.is_some_and(|current| current <= deadline) {
            return Ok(());
        }
        effects.arm_timer(deadline)?;
        self.scheduled = Some(deadline);
        Ok(())
    }
}

trait Effects {
    fn now(&self) -> Instant;
    fn clear_timer(&self);
    fn arm_timer(&self, deadline: Instant) -> io::Result<()>;
    fn start_scan(&self, scan: Scan) -> io::Result<()>;
}

struct WindowsCleanup<F> {
    hwnd: HWND,
    enqueue: F,
}

impl<F: Fn(Scan) -> io::Result<()>> Effects for WindowsCleanup<F> {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn clear_timer(&self) {
        unsafe { KillTimer(self.hwnd, TIMER) };
    }

    fn arm_timer(&self, deadline: Instant) -> io::Result<()> {
        let delay = deadline.saturating_duration_since(self.now());
        let wait = delay.as_millis().clamp(10, 0x7fff_ffff) as u32;
        if unsafe { SetTimer(self.hwnd, TIMER, wait, None) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn start_scan(&self, scan: Scan) -> io::Result<()> {
        (self.enqueue)(scan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct TestEffects {
        now: Instant,
        timer: Cell<Option<Instant>>,
        scans: RefCell<Vec<Scan>>,
    }

    impl Effects for TestEffects {
        fn now(&self) -> Instant {
            self.now
        }

        fn clear_timer(&self) {
            self.timer.set(None);
        }

        fn arm_timer(&self, deadline: Instant) -> io::Result<()> {
            self.timer.set(Some(deadline));
            Ok(())
        }

        fn start_scan(&self, scan: Scan) -> io::Result<()> {
            self.scans.borrow_mut().push(scan);
            Ok(())
        }
    }

    #[test]
    fn retention_changes_cancel_a_pending_scan_and_coalesce_the_followup() {
        let effects = TestEffects {
            now: Instant::now(),
            timer: Cell::default(),
            scans: RefCell::default(),
        };
        let mut cleanup = Cleanup::new(None);
        cleanup.update(Event::Request, &effects).unwrap();
        assert!(effects.scans.borrow().is_empty());

        cleanup
            .update(Event::RetentionChanged(1), &effects)
            .unwrap();
        let first = effects.scans.borrow_mut().pop().unwrap();
        cleanup
            .update(Event::RetentionChanged(10), &effects)
            .unwrap();
        cleanup
            .update(Event::RetentionChanged(1), &effects)
            .unwrap();
        cleanup.update(Event::Request, &effects).unwrap();
        assert!(!first.current.load(Ordering::Acquire));
        assert!(effects.scans.borrow().is_empty());

        let obsolete = effects.now() + Duration::from_secs(30);
        cleanup
            .update(Event::ScanFinished(Some(obsolete)), &effects)
            .unwrap();
        assert_eq!(effects.timer.get(), None);
        let next = effects.scans.borrow_mut().pop().unwrap();
        assert_eq!(next.minutes, 1);
        assert!(next.current.load(Ordering::Acquire));
        assert!(effects.scans.borrow().is_empty());

        let deadline = effects.now() + Duration::from_secs(60);
        cleanup
            .update(Event::ScanFinished(Some(deadline)), &effects)
            .unwrap();
        assert_eq!(effects.timer.get(), Some(deadline));
    }
}

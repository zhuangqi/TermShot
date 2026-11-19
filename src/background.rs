use crate::{
    capture::Image,
    storage::{SavedCapture, Storage},
};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_ENOUGH_QUOTA, HWND};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const READY: u32 = WM_APP + 4;
const SAVE_BYTE_LIMIT: usize = 64 * 1024 * 1024;
const SAVE_JOB_LIMIT: usize = 3;

enum Job {
    Save(Image),
    Clean,
}

pub enum Outcome {
    Saved(io::Result<SavedCapture>),
    Cleaned(Option<Instant>),
}

pub struct Background {
    jobs: mpsc::Sender<Job>,
    outcomes: mpsc::Receiver<(Outcome, usize)>,
    saving: usize,
    save_bytes: usize,
    retention: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
}

impl Background {
    pub fn start(owner: HWND, storage: Storage, retention: Option<u32>) -> io::Result<Self> {
        let (jobs, queue) = mpsc::channel();
        let (completed, outcomes) = mpsc::channel();
        let retention = Arc::new(AtomicU32::new(retention.unwrap_or(0)));
        let stopped = Arc::new(AtomicBool::new(false));
        let (policy, shutdown) = (Arc::clone(&retention), Arc::clone(&stopped));
        let owner = owner as usize;
        thread::Builder::new()
            .name("capture-storage".into())
            .spawn(move || {
                'jobs: for job in queue {
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let outcome = match job {
                        Job::Save(image) => {
                            (Outcome::Saved(storage.save_png(&image)), image.byte_len())
                        }
                        Job::Clean => {
                            let minutes = policy.load(Ordering::Acquire);
                            let started = Instant::now();
                            let deadline = (minutes != 0)
                                .then(|| {
                                    storage.clean(minutes, || {
                                        !shutdown.load(Ordering::Acquire)
                                            && policy.load(Ordering::Acquire) == minutes
                                    })
                                })
                                .flatten()
                                .and_then(|delay| {
                                    started.checked_add(Duration::from_millis(delay))
                                });
                            (Outcome::Cleaned(deadline), 0)
                        }
                    };
                    if completed.send(outcome).is_err() {
                        break;
                    }
                    while !shutdown.load(Ordering::Acquire) {
                        if unsafe { PostMessageW(owner as HWND, READY, 0, 0) } != 0 {
                            break;
                        }
                        if unsafe { GetLastError() } != ERROR_NOT_ENOUGH_QUOTA {
                            break 'jobs;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            })?;
        Ok(Self {
            jobs,
            outcomes,
            saving: 0,
            save_bytes: 0,
            retention,
            stopped,
        })
    }

    pub fn save_budget(&self) -> usize {
        match self.saving {
            // A single large multi-monitor screenshot can exceed the shared budget.
            0 => usize::MAX,
            SAVE_JOB_LIMIT.. => 0,
            _ => SAVE_BYTE_LIMIT.saturating_sub(self.save_bytes),
        }
    }

    pub fn is_saving(&self) -> bool {
        self.saving != 0
    }

    pub fn save(&mut self, image: Image) -> io::Result<()> {
        let bytes = image.byte_len();
        if bytes > self.save_budget() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "截图保存队列已满，请等待上一张截图保存后重试",
            ));
        }
        self.enqueue(Job::Save(image))?;
        self.saving += 1;
        self.save_bytes += bytes;
        Ok(())
    }

    pub fn clean(&self) -> io::Result<()> {
        self.enqueue(Job::Clean)
    }

    pub fn receive(&mut self) -> Option<Outcome> {
        let (outcome, bytes) = self.outcomes.try_recv().ok()?;
        if matches!(outcome, Outcome::Saved(_)) {
            self.saving -= 1;
            self.save_bytes -= bytes;
        }
        Some(outcome)
    }

    pub fn set_retention(&self, minutes: u32) {
        self.retention.store(minutes, Ordering::Release);
    }

    fn enqueue(&self, job: Job) -> io::Result<()> {
        self.jobs
            .send(job)
            .map_err(|_| io::Error::other("截图存储线程已退出"))
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

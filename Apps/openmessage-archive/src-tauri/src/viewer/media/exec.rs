//! Bounded job queues with their own worker threads (M5 spec §6.2).
//!
//! The protocol handler only parses, authorises and enqueues, so it never
//! blocks the thread that calls it (on WebKitGTK the GTK main loop). Each
//! executor has its own queue and workers, so a slow decode cannot delay
//! a stream chunk.
//!
//! - [`Order::NewestFirst`] (decode): a full queue drops its **oldest**
//!   entry to make room, and the newest job is taken first: the image the
//!   user scrolled to last is the one decoded next.
//! - [`Order::Fifo`] (stream): a full queue refuses the new job.
//!
//! A dropped or refused job is handed back to the caller, which answers
//! it; a job's reply answers `503` itself if it is dropped unanswered, so
//! no request is left hanging, not even when the session closes.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// Which queued job a worker takes next, and what a full queue does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    NewestFirst,
    Fifo,
}

/// What [`Queue::push`] did.
#[derive(Debug)]
pub enum Pushed<J> {
    /// Queued.
    Queued,
    /// Queued, and this oldest entry was dropped to make room.
    Displaced(J),
    /// Not queued: the queue is full (FIFO) or closed.
    Refused(J),
}

struct State<J> {
    jobs: VecDeque<J>,
    closed: bool,
}

/// A bounded queue shared by an executor's workers.
pub struct Queue<J> {
    state: Mutex<State<J>>,
    ready: Condvar,
    capacity: usize,
    order: Order,
}

impl<J> Queue<J> {
    pub fn new(capacity: usize, order: Order) -> Queue<J> {
        Queue {
            state: Mutex::new(State {
                jobs: VecDeque::with_capacity(capacity),
                closed: false,
            }),
            ready: Condvar::new(),
            capacity: capacity.max(1),
            order,
        }
    }

    /// Queue `job` without waiting.
    pub fn push(&self, job: J) -> Pushed<J> {
        let Ok(mut s) = self.state.lock() else {
            return Pushed::Refused(job);
        };
        if s.closed {
            return Pushed::Refused(job);
        }
        let displaced = if s.jobs.len() >= self.capacity {
            match self.order {
                Order::Fifo => return Pushed::Refused(job),
                Order::NewestFirst => s.jobs.pop_front(),
            }
        } else {
            None
        };
        s.jobs.push_back(job);
        drop(s);
        self.ready.notify_one();
        match displaced {
            Some(old) => Pushed::Displaced(old),
            None => Pushed::Queued,
        }
    }

    /// The next job, waiting for one; `None` once the queue is closed.
    pub fn pop(&self) -> Option<J> {
        let mut s = self.state.lock().ok()?;
        loop {
            if s.closed {
                return None;
            }
            let next = match self.order {
                Order::NewestFirst => s.jobs.pop_back(),
                Order::Fifo => s.jobs.pop_front(),
            };
            if let Some(j) = next {
                return Some(j);
            }
            s = self.ready.wait(s).ok()?;
        }
    }

    /// Close the queue and hand back what was waiting.
    pub fn close(&self) -> Vec<J> {
        let left = match self.state.lock() {
            Ok(mut s) => {
                s.closed = true;
                s.jobs.drain(..).collect()
            }
            Err(_) => Vec::new(),
        };
        self.ready.notify_all();
        left
    }

    /// Jobs waiting (for tests).
    pub fn len(&self) -> usize {
        self.state.lock().map(|s| s.jobs.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An executor: a queue and the threads that drain it.
pub struct Executor<J: Send + 'static> {
    queue: Arc<Queue<J>>,
    #[cfg_attr(not(test), allow(dead_code))]
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl<J: Send + 'static> Executor<J> {
    /// Start `workers.len()` threads named `name`; each runs its own
    /// `handle` (holding its own reader or helper) on every job it takes.
    pub fn start<W>(name: &str, capacity: usize, order: Order, workers: Vec<W>) -> Executor<J>
    where
        W: FnMut(J) + Send + 'static,
    {
        let queue = Arc::new(Queue::new(capacity, order));
        let mut handles = Vec::with_capacity(workers.len());
        for mut handle in workers {
            let q = Arc::clone(&queue);
            let spawned = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || {
                    while let Some(job) = q.pop() {
                        handle(job);
                    }
                });
            match spawned {
                Ok(h) => handles.push(h),
                Err(_) => tracing::warn!(executor = name, "media worker not started"),
            }
        }
        if handles.is_empty() {
            // Without a worker nothing would ever answer; refuse instead.
            drop(queue.close());
        }
        Executor {
            queue,
            workers: Mutex::new(handles),
        }
    }

    pub fn push(&self, job: J) -> Pushed<J> {
        self.queue.push(job)
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Close the queue (waiting jobs are dropped, so they answer `503`)
    /// and let the workers finish the job each holds. Does not wait.
    pub fn close(&self) {
        drop(self.queue.close());
    }

    /// Wait for the workers to stop (after [`Executor::close`]). Never
    /// called from a command: a worker may be inside a long decode.
    #[cfg(test)]
    pub fn join(&self) {
        let handles: Vec<JoinHandle<()>> = match self.workers.lock() {
            Ok(mut w) => w.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        for h in handles {
            let _ = h.join();
        }
    }
}

impl<J: Send + 'static> Drop for Executor<J> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn newest_first_drops_the_oldest_when_full() {
        let q = Queue::new(3, Order::NewestFirst);
        for i in 0..3 {
            assert!(matches!(q.push(i), Pushed::Queued));
        }
        assert!(matches!(q.push(3), Pushed::Displaced(0)));
        assert!(matches!(q.push(4), Pushed::Displaced(1)));
        assert_eq!(q.len(), 3);
        assert_eq!(q.pop(), Some(4));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(2));
    }

    #[test]
    fn fifo_refuses_when_full_and_keeps_order() {
        let q = Queue::new(2, Order::Fifo);
        assert!(matches!(q.push(1), Pushed::Queued));
        assert!(matches!(q.push(2), Pushed::Queued));
        assert!(matches!(q.push(3), Pushed::Refused(3)));
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
    }

    #[test]
    fn a_closed_queue_refuses_and_hands_back_its_jobs() {
        let q = Queue::new(4, Order::Fifo);
        assert!(matches!(q.push(1), Pushed::Queued));
        assert_eq!(q.close(), vec![1]);
        assert!(matches!(q.push(2), Pushed::Refused(2)));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn workers_drain_and_stop_on_close() {
        let (tx, rx) = mpsc::channel();
        let workers = (0..2)
            .map(|_| {
                let tx = tx.clone();
                move |j: u32| {
                    let _ = tx.send(j);
                }
            })
            .collect();
        let ex = Executor::start("test-exec", 8, Order::Fifo, workers);
        for i in 0..5 {
            assert!(matches!(ex.push(i), Pushed::Queued));
        }
        let mut got: Vec<u32> = (0..5).map(|_| rx.recv().expect("job")).collect();
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
        ex.close();
        assert!(matches!(ex.push(9), Pushed::Refused(9)));
    }
}

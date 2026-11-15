use super::super::{Error, Job, Reply, Result, SyncSender, Transport, mpsc};
use super::SLOTS;
use std::{sync::mpsc::Receiver, thread::JoinHandle};
pub(in super::super) struct Pool {
    senders: Vec<SyncSender<Job>>,
    receive: Option<Receiver<(usize, Reply)>>,
    threads: Vec<JoinHandle<()>>,
}
impl Pool {
    pub(in super::super) fn start(certificate: Option<&[u8]>, fence: std::sync::Arc<crate::worker::fence::AuthFence>) -> Result<Self> {
        let (replies, receive) = mpsc::sync_channel(SLOTS);
        let mut pool = Self { senders: Vec::new(), receive: Some(receive), threads: Vec::new() };
        for slot in 0..SLOTS {
            let transport = Transport::new(certificate)?;
            let fence = std::sync::Arc::clone(&fence);
            let (sender, jobs) = mpsc::sync_channel::<Job>(1);
            let replies = replies.clone();
            let thread = std::thread::Builder::new()
                .name(format!("komaphone-world-presence-{slot}"))
                .spawn(move || {
                    while let Ok(job) = jobs.recv() {
                        let result = job.call(&transport, &fence);
                        if replies.send((slot, Reply { generation: job.generation, sequence: job.sequence, action: job.action, result })).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|error| Error::Storage(error.to_string()))?;
            pool.senders.push(sender);
            pool.threads.push(thread);
        }
        Ok(pool)
    }
    pub(in super::super) fn senders(&self) -> &[SyncSender<Job>] {
        &self.senders
    }
    pub(in super::super) fn poll(&self) -> Option<(usize, Reply)> {
        self.receive.as_ref().and_then(|receive| receive.try_recv().ok())
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.senders.clear();
        // Drop bounded result receiver before joining. Always off the UI thread.
        self.receive = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropping_pool_releases_workers_blocked_on_full_result_queue() {
        let (replies, receive) = mpsc::sync_channel(1);
        let mut pool = Pool { senders: vec![], receive: Some(receive), threads: vec![] };
        let (started, ready) = mpsc::channel();
        for slot in 0..SLOTS {
            let (sender, jobs) = mpsc::sync_channel::<Job>(1);
            let replies = replies.clone();
            let started = started.clone();
            pool.senders.push(sender);
            pool.threads.push(std::thread::spawn(move || {
                started.send(()).unwrap();
                let canvas = photocraft_scene::CanvasId::new(&"1".repeat(32)).unwrap();
                let _ =
                    replies.send((slot, Reply { generation: 0, sequence: 1, action: super::super::Action::Peers { canvas }, result: Err(Error::Unavailable) }));
                let _ = jobs.recv();
            }));
        }
        for _ in 0..SLOTS {
            ready.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        }
        let start = std::time::Instant::now();
        drop(pool);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}

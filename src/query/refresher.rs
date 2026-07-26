use crossbeam_channel::{Receiver, Sender};

use crate::cache::Cache;
use crate::model::origin::PortKey;
use crate::moved::Moved;
use crate::query::makerunner::QueryCtx;
use crate::query::scanner::{self, ScanResult};

/// Events streamed back to the UI while a background refresh runs.
pub enum RefreshEvent {
    Progress { done: usize, discovered: usize },
    /// One scan finished; `batches` says how many submitted request batches
    /// it covered (queued requests are coalesced into a single scan).
    Done { result: Box<ScanResult>, batches: usize },
}

/// Handle to the background refresh thread. Send a batch of roots to
/// re-scan (usually the single toggled port); results stream back on `rx`.
pub struct Refresher {
    pub tx: Sender<Vec<PortKey>>,
    pub rx: Receiver<RefreshEvent>,
}

pub fn spawn(ctx: QueryCtx, jobs: usize, mut cache: Cache, moved: Moved) -> Refresher {
    let (req_tx, req_rx) = crossbeam_channel::unbounded::<Vec<PortKey>>();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<RefreshEvent>();
    std::thread::spawn(move || {
        while let Ok(mut roots) = req_rx.recv() {
            // Coalesce queued-up requests into one scan, counting them so the
            // UI's outstanding-batch counter stays balanced.
            let mut batches = 1;
            while let Ok(more) = req_rx.try_recv() {
                roots.extend(more);
                batches += 1;
            }
            let ev_progress = ev_tx.clone();
            let result = scanner::scan(&roots, &ctx, jobs, &mut cache, &moved, move |p| {
                let _ = ev_progress
                    .send(RefreshEvent::Progress { done: p.done, discovered: p.discovered });
            });
            if ev_tx.send(RefreshEvent::Done { result: Box::new(result), batches }).is_err() {
                return;
            }
        }
    });
    Refresher { tx: req_tx, rx: ev_rx }
}

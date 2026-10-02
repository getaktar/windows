//! File system watchers, one per watched folder (ReadDirectoryChangesW
//! through `notify`). Their events are only hints for the engine; when
//! Windows says events were lost (a buffer overflow), the folder is walked
//! again. Network shares are watched as well as polled. A missing
//! folder's nearest parent is watched instead, to notice it coming back.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use super::engine::WatchTarget;
use crate::core::SharedCore;

struct Running {
    target: WatchTarget,
    /// Dropping it stops watching.
    _watcher: RecommendedWatcher,
}

/// A watcher that couldn't start is tried again after this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Watchers {
    running: HashMap<String, Running>,
    failed: HashMap<String, Instant>,
}

impl Watchers {
    /// Starts and stops watchers to match `plan`: (folder ID, path,
    /// recursive) for each folder that should be watched now.
    pub fn sync(&mut self, core: &SharedCore, plan: Vec<WatchTarget>) {
        self.running.retain(|_, running| plan.contains(&running.target));
        for target in plan {
            let id = target.folder_id.clone();
            if self.running.contains_key(&id) || self.failed.get(&id).is_some_and(|at| at.elapsed() < RETRY_AFTER) {
                continue;
            }
            match start(core, &target) {
                Ok(watcher) => {
                    if !target.parent {
                        core.watched.engine.watch_started(&id);
                    }
                    self.failed.remove(&id);
                    self.running.insert(id, Running { target, _watcher: watcher });
                }
                Err(error) => {
                    // Tried again in a while; the reconcile scan catches
                    // what's missed meanwhile.
                    log::warn!("Could not watch {}: {error}", target.path.display());
                    if !target.parent {
                        core.watched.engine.watch_failed(&id, error.to_string());
                    }
                    self.failed.insert(id, Instant::now());
                }
            }
        }
    }
}

fn start(core: &SharedCore, target: &WatchTarget) -> notify::Result<RecommendedWatcher> {
    let app = core.app.clone();
    let folder_id = target.folder_id.clone();
    let parent = target.parent;
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let core = crate::core::core(&app);
        match result {
            // Something in a missing folder's parent: maybe it's back.
            Ok(_) if parent => core.watched.engine.hint(&folder_id, None),
            Ok(event) if event.need_rescan() => core.watched.engine.hint(&folder_id, None),
            Ok(event) => {
                if !matters(&event.kind) {
                    return;
                }
                for path in event.paths {
                    core.watched.engine.hint(&folder_id, Some(path));
                }
            }
            // The folder went away, or the watch broke: walking it again
            // finds out which.
            Err(_) => core.watched.engine.hint(&folder_id, None),
        }
        core.watched.wake();
    })?;
    let mode = if target.recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
    watcher.watch(&target.path, mode)?;
    Ok(watcher)
}

/// Created, written, renamed, or deleted: everything but reads. A deleted
/// (or renamed away) file's row goes gone, once it's checked to be missing.
fn matters(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_))
}

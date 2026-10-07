//! Decides what to upload from each watched folder. File system events are
//! only hints: every decision is made from the file on disk and the
//! ledger. A file goes through
//!
//! ```text
//! hint or reconcile scan -> rules -> write-complete check (size and mtime
//! settle, nothing writing to it) -> ledger (new, changed, renamed?) ->
//! batch window -> large-batch guard -> upload pipeline -> after upload
//! (verify, Recycle Bin / Uploaded/, hooks, clipboard, notifications)
//! ```
//!
//! The engine itself is synchronous and knows nothing of Tauri: the app
//! drives it with `tick` and the upload pipeline's results, through `Host`.
//! That's what lets the tests run it against a temporary folder.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::ledger::{self, Decision, Entry, Ledger, RemoteDelete, State};
use super::model::{AfterUpload, ClipboardPolicy, Hook, Modified, Notifications, Pause, WatchedFolder, WatchedFolderStore, WatchedFolders};
use super::platform;
use super::rules::{self, FileFacts, Rejection};
use crate::t;

/// More new files than this at once wait for the user's go-ahead.
pub const LARGE_BATCH: usize = 50;
/// Ready files are collected this long after the last one, then go up
/// together.
const BATCH_WINDOW: Duration = Duration::from_millis(500);
/// A file is ready once it hasn't been written to for this long.
const QUIET_MILLIS: i64 = 1000;
const FIRST_CHECK: Duration = Duration::from_millis(500);
const MAX_CHECK: Duration = Duration::from_secs(30);
/// The safety net, behind the file system events: every folder is walked
/// about this often, each at its own offset within `SCAN_SPREAD_SECS` so
/// they don't all wake up together.
const SCAN_EVERY: Duration = Duration::from_secs(60 * 60);
const SCAN_SPREAD_SECS: u64 = 10 * 60;
/// Network shares get their events too, but unreliably, so they're also
/// polled: every 30 s after a change, backing off to 5 minutes while
/// nothing does.
const POLL_FIRST: Duration = Duration::from_secs(30);
const POLL_MAX: Duration = Duration::from_secs(5 * 60);
/// A missing folder's parent is watched to notice it coming back; this is
/// the fallback for when no event says so (a drive mounted elsewhere).
const FOUND_CHECK: Duration = Duration::from_secs(2 * 60);
/// When something due couldn't be done (the folder was turned off), it's
/// looked at again this much later rather than straight away.
const DEFERRED: Duration = Duration::from_secs(60);
/// Automatic retries of uploads that failed on the network or the server.
const RETRY_MINUTES: [i64; 4] = [1, 5, 15, 60];
/// Files hashed at once.
const HASH_WORKERS: usize = 2;
/// More hinted paths than this before a tick: the folder is walked
/// instead.
const MAX_HINTS: usize = 10_000;

/// The moment, as both clocks: `at` for timers, `millis` (Unix) for file
/// times and the ledger. Passed in, so tests can move time along.
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub at: Instant,
    pub millis: i64,
}

impl Now {
    pub fn current() -> Self {
        Self { at: Instant::now(), millis: crate::util::now_millis() }
    }

    #[cfg(test)]
    pub fn after(self, duration: Duration) -> Self {
        Self { at: self.at + duration, millis: self.millis + duration.as_millis() as i64 }
    }
}

/// Things outside the folders that pause watching.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Conditions {
    pub on_battery: bool,
    pub metered: bool,
}

/// A file handed to the upload pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub folder_id: String,
    pub path: PathBuf,
    pub relative_path: String,
    pub batch_id: String,
    /// "Replace the uploaded file": the key it went to before.
    pub overwrite_key: Option<String>,
    /// The file's SHA-256 when the engine worked it out, so the pipeline
    /// doesn't read the file again for it.
    pub sha256: Option<String>,
}

/// A finished upload, as the pipeline reports it.
#[derive(Debug, Clone)]
pub struct Uploaded {
    pub object_key: String,
    pub url: String,
    /// What's copied: the public URL or a temporary link, formatted as the
    /// destination copies it.
    pub link: String,
    pub filename: String,
    pub destination_id: String,
    pub reused: bool,
    pub byte_size: i64,
    /// SHA-256 of the file as it is in the folder, when the pipeline worked
    /// it out (and the file went up unchanged), for telling a later change
    /// from a touch without reading it again.
    pub content_hash: Option<String>,
    /// The upload's short link, which `link` uses (`upload.shortUrl` in the
    /// hooks' payload).
    pub short_url: Option<String>,
}

/// What the engine needs from the app.
pub trait Host: Send + Sync + 'static {
    /// Queues the files with the folder's settings. Returns each one's job
    /// ID, or None when it couldn't be queued (no destination).
    fn enqueue(&self, folder: &WatchedFolder, files: &[Planned]) -> Vec<Option<String>>;
    /// Tries a failed job again; false when it's gone.
    fn retry_job(&self, job_id: &str) -> bool;
    fn notify(&self, title: &str, body: &str);
    /// Puts the links on the clipboard, one per line.
    fn copy(&self, links: &[String]);
    /// Something the windows show changed.
    fn changed(&self);
    /// Asked only while a pause option is on, and only again after the
    /// system says something changed (`Engine::conditions_changed`).
    fn conditions(&self) -> Conditions;
    /// Deletes a gone file's upload from the bucket, the way the Library
    /// deletes an upload, and reports back with `remote_deleted`.
    fn delete_remote(&self, folder: &WatchedFolder, entry: &Entry, batch_id: &str);
    /// Whether the upload history has the object from somewhere else than
    /// this folder (another upload went to the same key).
    fn used_elsewhere(&self, folder_id: &str, destination_id: &str, object_key: &str) -> bool;
    /// Whether Windows' energy saver is on, which puts off the hourly
    /// safety scan. Asked only when one is due.
    fn energy_saver(&self) -> bool;
}

/// Works out a file's SHA-256. Swappable, for tests.
pub type Hasher = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

/// A file to hash, and what it looked like when it was ready.
struct HashJob {
    folder_id: String,
    relative: String,
    path: PathBuf,
    facts: FileFacts,
}

struct HashDone {
    folder_id: String,
    relative: String,
    sha256: Option<String>,
    /// Still the same size and time once read: the hash is the file's.
    unchanged: bool,
}

#[derive(Default)]
struct PoolState {
    queue: VecDeque<HashJob>,
    running: usize,
    workers: usize,
    done: Vec<HashDone>,
    shutdown: bool,
}

/// Hashes files on up to `HASH_WORKERS` threads of its own, outside the
/// ticks; each result wakes the app's loop, and the next tick applies it.
struct HashPool {
    state: Mutex<PoolState>,
    work: Condvar,
    idle: Condvar,
    hasher: Hasher,
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl HashPool {
    fn new(hasher: Hasher) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(PoolState::default()), work: Condvar::new(), idle: Condvar::new(), hasher, waker: Mutex::new(None) })
    }

    fn submit(self: &Arc<Self>, job: HashJob) {
        let mut state = self.state.lock().unwrap();
        state.queue.push_back(job);
        if state.workers < HASH_WORKERS && state.workers < state.queue.len() + state.running {
            state.workers += 1;
            let pool = self.clone();
            std::thread::spawn(move || pool.work());
        }
        self.work.notify_one();
    }

    fn work(&self) {
        loop {
            let job = {
                let mut state = self.state.lock().unwrap();
                loop {
                    if state.shutdown {
                        return;
                    }
                    if let Some(job) = state.queue.pop_front() {
                        state.running += 1;
                        break job;
                    }
                    state = self.work.wait(state).unwrap();
                }
            };
            let sha256 = (self.hasher)(&job.path);
            let unchanged = std::fs::metadata(&job.path)
                .is_ok_and(|metadata| metadata.len() == job.facts.size && rules::mtime_millis(&metadata) == job.facts.mtime);
            {
                let mut state = self.state.lock().unwrap();
                state.running -= 1;
                state.done.push(HashDone { folder_id: job.folder_id, relative: job.relative, sha256, unchanged });
                if state.queue.is_empty() && state.running == 0 {
                    self.idle.notify_all();
                }
            }
            if let Some(waker) = self.waker.lock().unwrap().clone() {
                waker();
            }
        }
    }

    fn take_done(&self) -> Vec<HashDone> {
        std::mem::take(&mut self.state.lock().unwrap().done)
    }

    #[cfg(test)]
    fn wait_idle(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.queue.is_empty() || state.running > 0 {
            state = self.idle.wait(state).unwrap();
        }
    }

    fn shut_down(&self) {
        self.state.lock().unwrap().shutdown = true;
        self.work.notify_all();
    }
}

/// The write-complete check: a file is ready once its size and mtime are
/// the same on two checks in a row, it hasn't been written to for
/// `QUIET_MILLIS`, and nothing has it open for writing. Checks back off
/// from 1 s to 30 s while it stays the same, and start over when it
/// changes. A file never stops being checked: it stays "waiting".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stability {
    last: Option<(u64, i64)>,
    delay: Duration,
    pub next_check: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ready,
    Wait,
}

impl Stability {
    pub fn new(now: Now) -> Self {
        Self { last: None, delay: FIRST_CHECK, next_check: now.at + FIRST_CHECK }
    }

    /// Whether this look could make it ready, which is the only time it's
    /// worth asking whether something has the file open.
    pub fn could_be_ready(&self, size: u64, mtime: i64, now: Now) -> bool {
        self.last == Some((size, mtime)) && now.millis - mtime >= QUIET_MILLIS
    }

    pub fn observe(&mut self, size: u64, mtime: i64, unlocked: bool, now: Now) -> Verdict {
        let same = self.last == Some((size, mtime));
        self.last = Some((size, mtime));
        if same && now.millis - mtime >= QUIET_MILLIS && unlocked {
            return Verdict::Ready;
        }
        self.delay = if same { (self.delay * 2).min(MAX_CHECK) } else { FIRST_CHECK };
        // Unchanged and free, only too fresh: no point waiting past the
        // moment it's old enough.
        let until_quiet = Duration::from_millis((QUIET_MILLIS - (now.millis - mtime)).max(50) as u64);
        let wait = if same && unlocked { self.delay.min(until_quiet) } else { self.delay };
        self.next_check = now.at + wait;
        Verdict::Wait
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Watching,
    /// Watching is paused for every folder.
    Paused,
    /// The folder's own switch is off.
    Disabled,
    /// A Mac thing (its bookmark went stale); never on Windows.
    #[allow(dead_code)]
    AccessNeeded,
    NotFound,
    Error,
}

/// Why everything is paused, if it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PauseReason {
    User,
    Battery,
    Metered,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    #[serde(flatten)]
    pub folder: WatchedFolder,
    pub status: Status,
    pub waiting: usize,
    pub uploading: usize,
    pub failed: usize,
    pub awaiting_confirmation: usize,
    /// Uploads of deleted files on their way out of the bucket.
    pub deleting: usize,
    /// Deleted files whose uploads wait for the user's go-ahead (a large
    /// deletion).
    pub awaiting_delete_confirmation: usize,
    /// The file's name when exactly one waits, for "a.png was removed".
    pub awaiting_delete_name: Option<String>,
    /// Unix milliseconds.
    pub last_upload_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub paused: bool,
    pub pause_reason: Option<PauseReason>,
    pub paused_until: Pause,
    pub pause_on_battery: bool,
    pub pause_on_metered: bool,
    pub folders: Vec<FolderInfo>,
}

/// A folder to put a watcher on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    pub folder_id: String,
    pub path: PathBuf,
    pub recursive: bool,
    /// The folder is missing: this is its nearest existing parent, watched
    /// to notice it coming back.
    pub parent: bool,
}

/// A file still to be checked.
#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    stability: Stability,
    /// The user already said to upload it ("Upload Them").
    confirmed: bool,
}

/// A file that's ready and new (or changed), waiting for its batch.
#[derive(Debug, Clone)]
struct Ready {
    path: PathBuf,
    relative_path: String,
    facts: FileFacts,
    sha256: Option<String>,
    overwrite_key: Option<String>,
    confirmed: bool,
}

#[derive(Debug, Clone)]
struct Flight {
    job_id: Option<String>,
    batch_id: String,
}

#[derive(Debug, Default)]
struct Runtime {
    health: Option<Status>,
    candidates: BTreeMap<String, Candidate>,
    /// Ready files being hashed by the pool.
    hashing: HashMap<String, Looked>,
    ready: Vec<Ready>,
    last_ready: Option<Instant>,
    awaiting: Vec<Ready>,
    in_flight: HashMap<String, Flight>,
    /// Failed uploads' jobs, which are still in the panel for Retry.
    failed_jobs: HashMap<String, String>,
    last_error: Option<String>,
    /// When the folder is walked next; None: right away.
    next_scan: Option<Instant>,
    /// When a missing folder is looked for again; None: right away.
    next_found_check: Option<Instant>,
    /// The folder's path the cached facts below are about.
    path: PathBuf,
    /// Whether it's on a network share, worked out once.
    network: Option<bool>,
    /// A network share's poll interval.
    poll_every: Option<Duration>,
    /// A network share's folders as of the last poll.
    directories: rules::DirectoryCache,
    /// Its safety scan was put off for the energy saver.
    scan_deferred: bool,
}

#[derive(Debug, Default)]
struct Batch {
    folder_id: String,
    remaining: usize,
    links: Vec<String>,
    names: Vec<String>,
    failures: Vec<(String, String)>,
}

/// Remote deletes sent together, for one notification.
#[derive(Debug, Default)]
struct DeleteBatch {
    remaining: usize,
    names: Vec<String>,
}

/// File system events waiting for the next tick, kept apart from the
/// engine's state so a watcher never waits on a tick (or a tick on it).
#[derive(Debug, Default)]
struct Hints {
    /// Walk the folder: events were lost, or something big happened.
    rescan: bool,
    paths: HashSet<PathBuf>,
}

struct Inner {
    runtimes: HashMap<String, Runtime>,
    batches: HashMap<String, Batch>,
    delete_batches: HashMap<String, DeleteBatch>,
    last_purge: Option<Instant>,
    conditions: Conditions,
    /// Asked again at the next tick (the system said something changed).
    conditions_stale: bool,
    /// Whether it was paused at the last tick, to catch the moment it
    /// resumes.
    was_paused: bool,
    last_tick: Option<Now>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            runtimes: HashMap::new(),
            batches: HashMap::new(),
            delete_batches: HashMap::new(),
            last_purge: None,
            conditions: Conditions::default(),
            conditions_stale: true,
            was_paused: false,
            last_tick: None,
        }
    }
}

/// The engine's state is behind `inner`, which is never held across file
/// system access, hashing, or the ledger (SQLite): the windows read it from
/// the UI thread.
pub struct Engine<H: Host> {
    pub host: H,
    pub store: WatchedFolderStore,
    pub ledger: Ledger,
    inner: Mutex<Inner>,
    hints: Mutex<HashMap<String, Hints>>,
    hashes: Arc<HashPool>,
}

impl<H: Host> Drop for Engine<H> {
    fn drop(&mut self) {
        self.hashes.shut_down();
    }
}

/// What to do once an upload went through: the hooks to run.
pub struct AfterSuccess {
    pub folder: WatchedFolder,
    pub hooks: Vec<Hook>,
    pub payload: serde_json::Value,
}

/// A folder's hints, with which of the paths are folders (looked at before
/// the lock is taken).
struct Hinted {
    folder_id: String,
    rescan: bool,
    paths: Vec<(bool, PathBuf)>,
}

/// A candidate looked at in a tick, on its way to a decision.
#[derive(Debug)]
struct Looked {
    folder: WatchedFolder,
    relative: String,
    path: PathBuf,
    confirmed: bool,
    facts: FileFacts,
}

/// Each folder's own offset for its safety scan, so they spread out.
fn scan_offset(folder_id: &str) -> Duration {
    use std::hash::{Hash, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    folder_id.hash(&mut hasher);
    Duration::from_secs(hasher.finish() % SCAN_SPREAD_SECS)
}

/// The moment `millis` (Unix milliseconds) is, never further off than a
/// year and a day: a pause date from a file or a link can be anything, and
/// an `Instant` that far would overflow.
fn until(now: Now, millis: i64) -> Instant {
    const LONGEST: u64 = 366 * 24 * 60 * 60 * 1000;
    let wait = Duration::from_millis((millis.saturating_sub(now.millis).max(0) as u64).min(LONGEST));
    now.at.checked_add(wait).unwrap_or(now.at)
}

impl<H: Host> Engine<H> {
    pub fn new(host: H, store: WatchedFolderStore, ledger: Ledger) -> Self {
        let hasher: Hasher = Arc::new(|path| crate::util::sha256_file(path).ok());
        Self { host, store, ledger, inner: Mutex::new(Inner::default()), hints: Mutex::new(HashMap::new()), hashes: HashPool::new(hasher) }
    }

    #[cfg(test)]
    pub fn with_hasher(mut self, hasher: Hasher) -> Self {
        self.hashes.shut_down();
        self.hashes = HashPool::new(hasher);
        self
    }

    /// Called when a hash comes back, so the app's loop ticks to apply it.
    pub fn set_waker(&self, waker: Arc<dyn Fn() + Send + Sync>) {
        *self.hashes.waker.lock().unwrap() = Some(waker);
    }

    // MARK: - State

    fn paused_reason(&self, list: &WatchedFolders, conditions: Conditions, now: Now) -> Option<PauseReason> {
        if list.paused_until.is_active(now.millis) {
            Some(PauseReason::User)
        } else if list.pause_on_battery && conditions.on_battery {
            Some(PauseReason::Battery)
        } else if list.pause_on_metered && conditions.metered {
            Some(PauseReason::Metered)
        } else {
            None
        }
    }

    /// For the windows and the local API. The engine's state is copied out
    /// under its lock; the ledger's counts are read after it's released.
    pub fn overview(&self, now: Now) -> Overview {
        struct Live {
            health: Option<Status>,
            waiting: usize,
            uploading: usize,
            awaiting: usize,
            last_error: Option<String>,
        }
        let list = self.store.get();
        let (reason, live) = {
            let inner = self.inner.lock().unwrap();
            let reason = self.paused_reason(&list, inner.conditions, now);
            let live: Vec<Option<Live>> = list
                .folders
                .iter()
                .map(|folder| {
                    inner.runtimes.get(&folder.id).map(|runtime| Live {
                        health: runtime.health,
                        waiting: runtime.candidates.len() + runtime.hashing.len() + runtime.ready.len(),
                        uploading: runtime.in_flight.len(),
                        awaiting: runtime.awaiting.len(),
                        last_error: runtime.last_error.clone(),
                    })
                })
                .collect();
            (reason, live)
        };
        let folders = list
            .folders
            .iter()
            .zip(live)
            .map(|(folder, live)| {
                let summary = self.ledger.summary(&folder.id);
                let status = match (folder.enabled, reason, live.as_ref().and_then(|live| live.health)) {
                    (false, _, _) => Status::Disabled,
                    (_, _, Some(Status::NotFound)) => Status::NotFound,
                    (_, Some(_), _) => Status::Paused,
                    (_, _, Some(health)) => health,
                    _ => Status::Watching,
                };
                FolderInfo {
                    folder: folder.clone(),
                    status,
                    waiting: live.as_ref().map_or(0, |live| live.waiting),
                    uploading: live.as_ref().map_or(0, |live| live.uploading),
                    failed: summary.failed,
                    awaiting_confirmation: live.as_ref().map_or(0, |live| live.awaiting),
                    deleting: summary.deleting,
                    awaiting_delete_confirmation: summary.held,
                    awaiting_delete_name: (summary.held == 1)
                        .then(|| self.ledger.held_deletes(&folder.id).first().map(|entry| crate::util::last_component(&entry.relative_path).to_string()))
                        .flatten(),
                    last_upload_at: summary.last_upload_at,
                    last_error: live.and_then(|live| live.last_error),
                }
            })
            .collect();
        Overview {
            paused: reason.is_some(),
            pause_reason: reason,
            paused_until: list.paused_until,
            pause_on_battery: list.pause_on_battery,
            pause_on_metered: list.pause_on_metered,
            folders,
        }
    }

    /// The folders that should have a watcher on them now, with whether it
    /// goes into subfolders; a missing folder's nearest existing parent, to
    /// notice it coming back. Network shares are watched too (their local
    /// changes are reported) besides being polled.
    pub fn watch_plan(&self, now: Now) -> Vec<WatchTarget> {
        let list = self.store.get();
        let missing: HashSet<String> = {
            let inner = self.inner.lock().unwrap();
            if self.paused_reason(&list, inner.conditions, now).is_some() {
                return Vec::new();
            }
            inner.runtimes.iter().filter(|(_, runtime)| runtime.health == Some(Status::NotFound)).map(|(id, _)| id.clone()).collect()
        };
        list.folders
            .iter()
            .filter(|folder| folder.enabled)
            .filter_map(|folder| {
                if !missing.contains(&folder.id) {
                    return Some(WatchTarget { folder_id: folder.id.clone(), path: folder.path.clone(), recursive: folder.recursive(), parent: false });
                }
                let parent = folder.path.ancestors().skip(1).find(|ancestor| ancestor.is_dir())?;
                Some(WatchTarget { folder_id: folder.id.clone(), path: parent.to_path_buf(), recursive: false, parent: true })
            })
            .collect()
    }

    /// A folder's watcher couldn't start (or stopped).
    pub fn watch_failed(&self, folder_id: &str, message: String) {
        {
            let mut inner = self.inner.lock().unwrap();
            let runtime = inner.runtimes.entry(folder_id.to_string()).or_default();
            if runtime.health == Some(Status::NotFound) {
                return;
            }
            runtime.health = Some(Status::Error);
            runtime.last_error = Some(message);
        }
        self.host.changed();
    }

    pub fn watch_started(&self, folder_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        let runtime = inner.runtimes.entry(folder_id.to_string()).or_default();
        if runtime.health == Some(Status::Error) {
            runtime.health = None;
            runtime.last_error = None;
        }
    }

    // MARK: - Hints

    /// Something happened to `path` in the folder (or, with None, events
    /// were lost and the whole folder is walked again). Called from the
    /// watchers' threads: only its own small lock, nothing else.
    pub fn hint(&self, folder_id: &str, path: Option<PathBuf>) {
        let mut hints = self.hints.lock().unwrap();
        let entry = hints.entry(folder_id.to_string()).or_default();
        match path {
            Some(_) if entry.rescan => {}
            Some(path) if entry.paths.len() < MAX_HINTS => {
                entry.paths.insert(path);
            }
            _ => {
                entry.rescan = true;
                entry.paths = HashSet::new();
            }
        }
    }

    /// Every folder is walked again soon: launch, waking up, the network
    /// coming back.
    pub fn rescan_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.conditions_stale = true;
        for runtime in inner.runtimes.values_mut() {
            runtime.next_scan = None;
            runtime.next_found_check = None;
        }
    }

    /// The energy saver went on or off: safety scans put off for it run now
    /// (when it's off; otherwise they're put off again).
    pub fn energy_saver_changed(&self) {
        let mut inner = self.inner.lock().unwrap();
        for runtime in inner.runtimes.values_mut().filter(|runtime| runtime.scan_deferred) {
            runtime.scan_deferred = false;
            runtime.next_scan = Some(Instant::now());
        }
    }

    /// The power source or the network's cost changed: asked again at the
    /// next tick (only while a pause option uses them).
    pub fn conditions_changed(&self) {
        self.inner.lock().unwrap().conditions_stale = true;
    }

    // MARK: - Ticking

    /// Does whatever is due, and returns when it should be called again:
    /// the earliest real deadline (a file's next check, a batch window, a
    /// retry, a delete, the hourly scan), or None when nothing is
    /// scheduled at all. The app calls it then, or when something wakes it.
    pub fn tick(&self, now: Now) -> Option<Instant> {
        let list = self.store.get();
        let hints = std::mem::take(&mut *self.hints.lock().unwrap());
        if list.folders.is_empty() {
            self.inner.lock().unwrap().runtimes.clear();
            return None;
        }
        // The system is asked outside the lock, and only when it matters.
        let wants_conditions = list.pause_on_battery || list.pause_on_metered;
        let ask = wants_conditions && self.inner.lock().unwrap().conditions_stale;
        let asked = ask.then(|| self.host.conditions());
        // Which hinted paths are folders (one was created or moved in).
        let hints: Vec<Hinted> = hints
            .into_iter()
            .map(|(folder_id, hints)| Hinted {
                folder_id,
                rescan: hints.rescan,
                paths: hints.paths.into_iter().map(|path| (path.is_dir(), path)).collect(),
            })
            .collect();

        let mut notify_changed = false;
        let mut due: Vec<(WatchedFolder, String, PathBuf, bool)> = Vec::new();
        let mut walks: Vec<WatchedFolder> = Vec::new();
        let mut found_checks: Vec<WatchedFolder> = Vec::new();
        let mut safety_scans: Vec<WatchedFolder> = Vec::new();
        let mut flush: Vec<(WatchedFolder, Vec<Ready>)> = Vec::new();
        let paused;
        let purge;
        {
            let mut inner = self.inner.lock().unwrap();
            // A tick straight after waking up from sleep catches up on
            // everything.
            let slept = inner.last_tick.is_some_and(|last| {
                let wall = now.millis - last.millis;
                let monotonic = now.at.saturating_duration_since(last.at).as_millis() as i64;
                wall - monotonic > 60_000
            });
            inner.last_tick = Some(now);
            match asked {
                Some(conditions) => {
                    inner.conditions = conditions;
                    inner.conditions_stale = false;
                }
                None if !wants_conditions => inner.conditions = Conditions::default(),
                None => {}
            }
            paused = self.paused_reason(&list, inner.conditions, now).is_some();
            purge = inner.last_purge.is_none_or(|last| now.at.saturating_duration_since(last) >= Duration::from_secs(60));
            if purge {
                inner.last_purge = Some(now.at);
            }
            if paused != inner.was_paused {
                inner.was_paused = paused;
                notify_changed = true;
                for runtime in inner.runtimes.values_mut() {
                    // Picked up again by the reconcile scan on resume.
                    runtime.candidates.clear();
                    runtime.hashing.clear();
                    runtime.ready.clear();
                    runtime.next_scan = None;
                }
            }
            if slept {
                for runtime in inner.runtimes.values_mut() {
                    runtime.next_scan = None;
                    runtime.next_found_check = None;
                }
            }
            inner.runtimes.retain(|id, _| list.folders.iter().any(|folder| folder.id == *id));
            for folder in &list.folders {
                let runtime = inner.runtimes.entry(folder.id.clone()).or_default();
                if runtime.path != folder.path {
                    runtime.path = folder.path.clone();
                    runtime.network = None;
                    runtime.poll_every = None;
                    runtime.directories.clear();
                }
            }

            for Hinted { folder_id, rescan, paths } in hints {
                let Some(folder) = list.folders.iter().find(|folder| folder.id == folder_id) else { continue };
                if paused || !folder.enabled {
                    continue;
                }
                let Some(runtime) = inner.runtimes.get_mut(&folder_id) else { continue };
                if runtime.health == Some(Status::NotFound) {
                    // Its parent saw something: maybe it's back.
                    runtime.next_found_check = None;
                    continue;
                }
                if rescan {
                    runtime.next_scan = None;
                }
                for (is_dir, path) in paths {
                    if is_dir {
                        runtime.next_scan = None;
                        continue;
                    }
                    let Some(relative) = rules::relative_path(&folder.path, &path) else { continue };
                    if rules::in_scope(folder.subfolders, &relative) && !runtime.in_flight.contains_key(&relative) && !runtime.hashing.contains_key(&relative) {
                        runtime.candidates.entry(relative).or_insert_with(|| Candidate { path, stability: Stability::new(now), confirmed: false });
                        notify_changed = true;
                    }
                }
            }

            for folder in &list.folders {
                let Some(runtime) = inner.runtimes.get_mut(&folder.id) else { continue };
                if !folder.enabled || paused {
                    continue;
                }
                if runtime.health == Some(Status::NotFound) {
                    if runtime.next_found_check.is_none_or(|at| at <= now.at) {
                        runtime.next_found_check = Some(now.at + FOUND_CHECK);
                        found_checks.push(folder.clone());
                    }
                    continue;
                }
                match runtime.next_scan {
                    Some(at) if at > now.at => {}
                    scheduled => {
                        // Set properly once the walk is done.
                        runtime.next_scan = Some(now.at + SCAN_EVERY);
                        // The hourly safety scan waits for the energy saver;
                        // a rescan someone asked for (and a share's poll)
                        // doesn't.
                        if scheduled.is_some() && runtime.network != Some(true) {
                            safety_scans.push(folder.clone());
                        } else {
                            walks.push(folder.clone());
                        }
                    }
                }
                for (relative, candidate) in &runtime.candidates {
                    if candidate.stability.next_check <= now.at {
                        due.push((folder.clone(), relative.clone(), candidate.path.clone(), candidate.confirmed));
                    }
                }
                if !runtime.ready.is_empty() && runtime.last_ready.is_some_and(|last| now.at.saturating_duration_since(last) >= BATCH_WINDOW) {
                    flush.push((folder.clone(), std::mem::take(&mut runtime.ready)));
                }
            }
        }

        if !safety_scans.is_empty() {
            if self.host.energy_saver() {
                // Run once it's off (`energy_saver_changed`), or an hour on.
                let mut inner = self.inner.lock().unwrap();
                for folder in &safety_scans {
                    if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                        runtime.scan_deferred = true;
                    }
                }
            } else {
                walks.extend(safety_scans);
            }
        }
        // A missing folder that's there again is walked right away.
        for folder in found_checks {
            if folder.path.is_dir() {
                let mut inner = self.inner.lock().unwrap();
                if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                    runtime.health = None;
                    runtime.last_error = None;
                    runtime.next_scan = Some(now.at + SCAN_EVERY);
                }
                drop(inner);
                walks.push(folder);
                notify_changed = true;
            }
        }
        for folder in walks {
            notify_changed |= self.reconcile(&folder, now, false);
        }
        notify_changed |= self.apply_hashes(now);
        if !due.is_empty() {
            notify_changed |= self.process_candidates(due, now);
        }
        for (folder, ready) in flush {
            self.flush(&folder, ready);
            notify_changed = true;
        }
        if !paused {
            for entry in self.ledger.due_retries(now.millis) {
                self.retry_entry(&entry, now);
                notify_changed = true;
            }
        }
        notify_changed |= self.process_deletes(&list, now, paused);
        if purge {
            self.ledger.purge_gone(now.millis);
        }
        if notify_changed {
            self.host.changed();
        }
        self.next_deadline(&list, now)
    }

    /// The earliest moment something is due, from the engine's state and
    /// the ledger. Something overdue that couldn't be done this tick (its
    /// folder is off) is looked at again a minute later, never in a loop.
    fn next_deadline(&self, list: &WatchedFolders, now: Now) -> Option<Instant> {
        if list.folders.is_empty() {
            return None;
        }
        let mut next: Option<Instant> = None;
        let mut consider = |at: Instant| next = Some(next.map_or(at, |next: Instant| next.min(at)));
        if let Pause::Until(until_millis) = list.paused_until {
            if until_millis > now.millis {
                consider(until(now, until_millis));
            }
        }
        let paused;
        {
            let inner = self.inner.lock().unwrap();
            paused = self.paused_reason(list, inner.conditions, now).is_some();
            if !paused {
                for folder in list.folders.iter().filter(|folder| folder.enabled) {
                    let Some(runtime) = inner.runtimes.get(&folder.id) else {
                        consider(now.at);
                        continue;
                    };
                    if runtime.health == Some(Status::NotFound) {
                        consider(runtime.next_found_check.unwrap_or(now.at));
                        continue;
                    }
                    consider(runtime.next_scan.unwrap_or(now.at));
                    for candidate in runtime.candidates.values() {
                        consider(candidate.stability.next_check);
                    }
                    if let Some(last) = runtime.last_ready.filter(|_| !runtime.ready.is_empty()) {
                        consider(last + BATCH_WINDOW);
                    }
                }
            }
        }
        if !paused {
            if let Some(due) = self.ledger.next_due() {
                consider(if due <= now.millis { now.at + DEFERRED } else { until(now, due) });
            }
        }
        next.map(|at| at.max(now.at + Duration::from_millis(10)))
    }

    /// Walks the folder and makes a candidate of every file the ledger
    /// doesn't know yet (or knows to have changed), and marks rows whose
    /// file is gone. `confirmed` when the user asked for the folder's
    /// existing files. Returns whether anything was found. The directory
    /// listing has each file's size and time, so nothing is opened, and
    /// the engine's lock is only taken to read and update its state.
    fn reconcile(&self, folder: &WatchedFolder, now: Now, confirmed: bool) -> bool {
        if !folder.path.is_dir() {
            let mut inner = self.inner.lock().unwrap();
            let runtime = inner.runtimes.entry(folder.id.clone()).or_default();
            let changed = runtime.health != Some(Status::NotFound);
            runtime.health = Some(Status::NotFound);
            runtime.next_found_check = Some(now.at + FOUND_CHECK);
            runtime.candidates.clear();
            runtime.ready.clear();
            return changed;
        }
        let cached_network = self.inner.lock().unwrap().runtimes.get(&folder.id).and_then(|runtime| runtime.network);
        let network = cached_network.unwrap_or_else(|| platform::is_network_path(&folder.path));
        let mut directories = if network {
            self.inner.lock().unwrap().runtimes.get_mut(&folder.id).map(|runtime| std::mem::take(&mut runtime.directories)).unwrap_or_default()
        } else {
            rules::DirectoryCache::new()
        };
        let scan = rules::scan(folder, network.then_some(&mut directories));
        let known: HashMap<String, Entry> =
            self.ledger.entries(&folder.id).into_iter().map(|entry| (entry.relative_path.clone(), entry)).collect();
        let busy: HashSet<String> = {
            let inner = self.inner.lock().unwrap();
            inner
                .runtimes
                .get(&folder.id)
                .map(|runtime| {
                    runtime
                        .in_flight
                        .keys()
                        .chain(runtime.candidates.keys())
                        .chain(runtime.hashing.keys())
                        .cloned()
                        .chain(runtime.ready.iter().chain(&runtime.awaiting).map(|ready| ready.relative_path.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let walked: HashSet<&str> = scan.files.iter().map(|listed| listed.relative.as_str()).collect();
        let mut fresh = Vec::new();
        for listed in &scan.files {
            if busy.contains(&listed.relative) {
                continue;
            }
            // Handled and unchanged: no need to look closer. (A gone row
            // with a file at its path again always is.)
            if let Some(entry) = known.get(&listed.relative) {
                if !matches!(entry.state, State::Pending | State::Gone) && entry.size == listed.size && entry.mtime == listed.mtime {
                    continue;
                }
            }
            fresh.push((listed.relative.clone(), listed.path.clone()));
        }
        // Handled files no longer there were deleted (or moved away). Only
        // the rows the listing didn't have are looked at, one by one.
        let mut gone = Vec::new();
        let mut forgotten = Vec::new();
        if scan.complete && self.may_mark_gone(folder) {
            for entry in known.values() {
                let relative = entry.relative_path.as_str();
                if entry.state == State::Gone
                    || walked.contains(relative)
                    || busy.contains(relative)
                    || !rules::in_scope(folder.subfolders, relative)
                    || scan.unlisted.contains(rules::subpath(relative))
                {
                    continue;
                }
                if is_missing(&folder.path.join(relative)) {
                    match entry.state {
                        State::Failed => forgotten.push(entry.relative_path.clone()),
                        _ => gone.push(entry.gone(now.millis, folder.deletes_remote())),
                    }
                }
            }
        }
        self.ledger.put_all(&gone);
        for relative in &forgotten {
            self.ledger.remove(&folder.id, relative);
        }
        let found = !fresh.is_empty();
        let mut inner = self.inner.lock().unwrap();
        let runtime = inner.runtimes.entry(folder.id.clone()).or_default();
        runtime.network = Some(network);
        for (relative, path) in fresh {
            if !runtime.in_flight.contains_key(&relative) {
                runtime.candidates.entry(relative).or_insert_with(|| Candidate { path, stability: Stability::new(now), confirmed });
            }
        }
        if network {
            runtime.directories = directories;
            let changed = found || !gone.is_empty();
            let every = match (changed, runtime.poll_every) {
                (false, Some(every)) => (every * 2).min(POLL_MAX),
                _ => POLL_FIRST,
            };
            runtime.poll_every = Some(every);
            runtime.next_scan = Some(now.at + every);
        } else {
            runtime.next_scan = Some(now.at + SCAN_EVERY + scan_offset(&folder.id));
        }
        found || !gone.is_empty() || !forgotten.is_empty()
    }

    /// Rows only go gone when the folder itself is there, watched, and can
    /// be read: never for an unplugged drive, an offline share, a folder
    /// that can't be watched, where everything would look deleted, or one
    /// that's paused (the scan on resume catches up).
    fn may_mark_gone(&self, folder: &WatchedFolder) -> bool {
        let health = self.inner.lock().unwrap().runtimes.get(&folder.id).and_then(|runtime| runtime.health);
        let list = self.store.get();
        let paused = list.paused_until.is_active(crate::util::now_millis());
        folder.enabled && !paused && !matches!(health, Some(Status::NotFound | Status::Error | Status::AccessNeeded)) && folder.path.is_dir()
    }

    /// The candidates due this tick: each looked at once (one handle) and
    /// put through the write-complete check. A ready file whose folder
    /// tells changes from touches goes to the hash pool, off the tick (a
    /// big file never holds up the others); the rest are decided against
    /// the ledger now. The engine's lock is only taken to update its state.
    fn process_candidates(&self, due: Vec<(WatchedFolder, String, PathBuf, bool)>, now: Now) -> bool {
        let mut changed = false;
        let mut ready: Vec<Looked> = Vec::new();
        for (folder, relative, path, confirmed) in due {
            let facts = match rules::check(&folder, &path, &relative) {
                Ok(facts) => facts,
                Err(rejected) => {
                    self.rejected(&folder, &relative, &path, rejected, now);
                    changed = true;
                    continue;
                }
            };
            let could_be_ready = {
                let inner = self.inner.lock().unwrap();
                let Some(candidate) = inner.runtimes.get(&folder.id).and_then(|runtime| runtime.candidates.get(&relative)) else { continue };
                candidate.stability.could_be_ready(facts.size, facts.mtime, now)
            };
            // Only asked when it could be ready: it opens the file.
            let unlocked = !could_be_ready || platform::is_unlocked(&path);
            let verdict = {
                let mut inner = self.inner.lock().unwrap();
                let Some(candidate) = inner.runtimes.get_mut(&folder.id).and_then(|runtime| runtime.candidates.get_mut(&relative)) else { continue };
                candidate.stability.observe(facts.size, facts.mtime, unlocked, now)
            };
            if verdict == Verdict::Ready {
                ready.push(Looked { folder, relative, path, confirmed, facts });
            }
        }
        if ready.is_empty() {
            return changed;
        }
        let (hashed, decided): (Vec<Looked>, Vec<Looked>) = ready.into_iter().partition(|looked| looked.folder.modified != Modified::Ignore);
        {
            let mut inner = self.inner.lock().unwrap();
            for looked in hashed {
                let Some(runtime) = inner.runtimes.get_mut(&looked.folder.id) else { continue };
                runtime.candidates.remove(&looked.relative);
                self.hashes.submit(HashJob { folder_id: looked.folder.id.clone(), relative: looked.relative.clone(), path: looked.path.clone(), facts: looked.facts });
                runtime.hashing.insert(looked.relative.clone(), looked);
            }
        }
        for looked in decided {
            self.settle(looked, None, now);
        }
        true
    }

    /// Hashes that came back since the last tick: each file decided, unless
    /// it changed while it was being read (then it's checked again), or
    /// it's no longer wanted (its folder was paused, changed, or removed).
    fn apply_hashes(&self, now: Now) -> bool {
        let done = self.hashes.take_done();
        let mut changed = false;
        for result in done {
            let looked = {
                let mut inner = self.inner.lock().unwrap();
                let Some(runtime) = inner.runtimes.get_mut(&result.folder_id) else { continue };
                let Some(looked) = runtime.hashing.remove(&result.relative) else { continue };
                if !result.unchanged {
                    runtime.candidates.insert(looked.relative.clone(), Candidate { path: looked.path, stability: Stability::new(now), confirmed: looked.confirmed });
                    changed = true;
                    continue;
                }
                looked
            };
            self.settle(looked, result.sha256, now);
            changed = true;
        }
        changed
    }

    /// A ready file decided against the ledger, and taken out of the
    /// candidates: into its batch when it's to be uploaded.
    fn settle(&self, looked: Looked, sha256: Option<String>, now: Now) {
        let outcome = self.decide(&looked, sha256, now);
        let mut inner = self.inner.lock().unwrap();
        let Some(runtime) = inner.runtimes.get_mut(&looked.folder.id) else { return };
        runtime.candidates.remove(&looked.relative);
        if let Some(file) = outcome {
            runtime.ready.push(file);
            runtime.last_ready = Some(now.at);
        }
    }

    /// Waits until the hash pool has nothing queued or running (tests move
    /// their clock only once hashes are back).
    #[cfg(test)]
    pub fn wait_for_hashes(&self) {
        self.hashes.wait_idle();
    }

    /// A ready file against the ledger: what it is, and the ledger brought
    /// up to date. Returns the file when it's to be uploaded.
    fn decide(&self, looked: &Looked, sha256: Option<String>, now: Now) -> Option<Ready> {
        let Looked { folder, relative, path, confirmed, facts } = looked;
        let existing = self.ledger.get(&folder.id, relative);
        let others = match (&existing, facts.file_id) {
            (Some(entry), _) if entry.state != State::Gone => Vec::new(),
            (_, Some(file_id)) => self.ledger.by_file_id(&folder.id, file_id),
            _ => Vec::new(),
        };
        let decision =
            ledger::decide(folder.modified, existing.as_ref(), facts, || sha256.clone(), &others, |other| folder.path.join(other).exists(), now.millis);
        // Something is at a deleted file's path again: its upload stays
        // (the new file's upload may even go to the same key).
        let existing = existing.map(|entry| match entry.state {
            State::Gone if entry.remote_delete.is_some_and(|delete| delete != RemoteDelete::Running) => {
                let cancelled = Entry { remote_delete: None, ..entry };
                self.ledger.put(&cancelled);
                cancelled
            }
            _ => entry,
        });
        let overwrite_key = match decision {
            Decision::Handled => {
                // Saved again without changes: remember its new date, so it
                // isn't hashed again every time. A deleted file written
                // again (an atomic save) is back.
                if let Some(entry) = existing {
                    let returned = entry.state == State::Gone;
                    let mut entry = if returned { entry.restored() } else { entry };
                    if returned || (!entry.same_contents_as(facts) && entry.state != State::Failed) {
                        entry.size = facts.size;
                        entry.mtime = facts.mtime;
                        entry.file_id = facts.file_id.or(entry.file_id);
                        if entry.sha256.is_none() {
                            entry.sha256.clone_from(&sha256);
                        }
                        self.ledger.put(&entry);
                    }
                }
                return None;
            }
            Decision::Renamed { from } => {
                // The same file again (moved or renamed, maybe back to where
                // it was): its row follows it, and its upload stays.
                if from != *relative {
                    self.ledger.rename(&folder.id, &from, relative);
                }
                if let Some(entry) = self.ledger.get(&folder.id, relative).filter(|entry| entry.state == State::Gone) {
                    self.ledger.put(&entry.restored());
                }
                return None;
            }
            Decision::Changed(entry) if folder.modified == Modified::Overwrite => entry.object_key.clone(),
            Decision::Changed(_) | Decision::New => None,
        };
        Some(Ready { path: path.clone(), relative_path: relative.clone(), facts: *facts, sha256, overwrite_key, confirmed: *confirmed })
    }

    /// A candidate that isn't a file to upload (any more). Gone (renamed,
    /// deleted): its row goes gone; the rename's other half, if any, comes
    /// as its own hint.
    fn rejected(&self, folder: &WatchedFolder, relative: &str, path: &Path, rejected: Rejection, now: Now) {
        let in_flight = {
            let mut inner = self.inner.lock().unwrap();
            inner.runtimes.get_mut(&folder.id).is_some_and(|runtime| {
                runtime.candidates.remove(relative);
                // A held file that's gone leaves the ask, which would
                // otherwise hold every later file up behind it.
                if rejected == Rejection::Missing && is_missing(path) {
                    runtime.awaiting.retain(|file| file.relative_path != relative);
                }
                runtime.in_flight.contains_key(relative)
            })
        };
        if rejected != Rejection::Missing || in_flight || !is_missing(path) || !self.may_mark_gone(folder) {
            return;
        }
        match self.ledger.get(&folder.id, relative) {
            Some(entry) if entry.state == State::Failed => self.ledger.remove(&folder.id, relative),
            Some(entry) if entry.state != State::Gone => self.ledger.put(&entry.gone(now.millis, folder.deletes_remote())),
            Some(_) => {}
            // A folder: what was in it is looked for again.
            None => {
                if self.ledger.has_prefix(&folder.id, &format!("{relative}/")) {
                    let mut inner = self.inner.lock().unwrap();
                    if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                        runtime.next_scan = None;
                    }
                }
            }
        }
    }

    /// A batch whose window closed: held for confirmation when it's large,
    /// uploaded otherwise. Asked once, when the first large batch arrives.
    fn flush(&self, folder: &WatchedFolder, ready: Vec<Ready>) {
        let unconfirmed = ready.iter().filter(|file| !file.confirmed).count();
        let held = {
            let mut inner = self.inner.lock().unwrap();
            let Some(runtime) = inner.runtimes.get_mut(&folder.id) else { return };
            // Held files deleted without an event don't keep the ask open.
            runtime.awaiting.retain(|file| !is_missing(&file.path));
            let was_empty = runtime.awaiting.is_empty();
            if unconfirmed > LARGE_BATCH || !was_empty {
                runtime.awaiting.extend(ready.iter().cloned());
                Some(was_empty.then_some(runtime.awaiting.len()))
            } else {
                None
            }
        };
        match held {
            Some(Some(count)) => self.host.notify(&t!("{0} new files in {1}", count, folder.name), &t!("Open Aktar to upload or skip them.")),
            Some(None) => {}
            None => self.enqueue(folder, ready),
        }
    }

    fn enqueue(&self, folder: &WatchedFolder, files: Vec<Ready>) {
        if files.is_empty() {
            return;
        }
        let batch_id = crate::util::new_id();
        let planned: Vec<Planned> = files
            .iter()
            .map(|file| Planned {
                folder_id: folder.id.clone(),
                path: file.path.clone(),
                relative_path: file.relative_path.clone(),
                batch_id: batch_id.clone(),
                overwrite_key: file.overwrite_key.clone(),
                sha256: file.sha256.clone(),
            })
            .collect();
        let pending: Vec<Entry> = files
            .iter()
            .map(|file| {
                let previous = self.ledger.get(&folder.id, &file.relative_path);
                Entry {
                    sha256: file.sha256.clone(),
                    object_key: file.overwrite_key.clone(),
                    attempts: previous.map_or(0, |previous| previous.attempts),
                    ..Entry::new(&folder.id, &file.relative_path, file.facts, State::Pending)
                }
            })
            .collect();
        self.ledger.put_all(&pending);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.batches.insert(batch_id.clone(), Batch { folder_id: folder.id.clone(), remaining: files.len(), ..Batch::default() });
            if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                for file in &planned {
                    runtime.in_flight.insert(file.relative_path.clone(), Flight { job_id: None, batch_id: batch_id.clone() });
                }
            }
        }
        let jobs = self.host.enqueue(folder, &planned);
        let mut not_queued = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                for (file, job) in planned.iter().zip(jobs.iter().chain(std::iter::repeat(&None))) {
                    match job {
                        Some(job_id) => {
                            if let Some(flight) = runtime.in_flight.get_mut(&file.relative_path) {
                                flight.job_id = Some(job_id.clone());
                            }
                        }
                        None => not_queued.push(file.relative_path.clone()),
                    }
                }
            }
        }
        for relative in not_queued {
            self.failed(&folder.id, &relative, t!("No destination to upload to. Add one in Settings."), false, Now::current());
        }
    }

    // MARK: - Results from the pipeline

    /// An upload went through (or an earlier upload's link was reused).
    /// `verified` when the object was found in the bucket with the size
    /// that was uploaded: only then may the original be moved away.
    pub fn succeeded(&self, folder_id: &str, relative: &str, batch_id: &str, uploaded: &Uploaded, verified: bool, now: Now) -> Option<AfterSuccess> {
        let folder = self.store.folder(folder_id)?;
        let path = folder.path.join(relative);
        let mut entry = self.ledger.get(folder_id, relative).unwrap_or_else(|| {
            let facts = std::fs::metadata(&path)
                .map(|metadata| FileFacts { size: metadata.len(), mtime: rules::mtime_millis(&metadata), file_id: None })
                .unwrap_or(FileFacts { size: 0, mtime: 0, file_id: None });
            Entry::new(folder_id, relative, facts, State::Pending)
        });
        entry.state = State::Uploaded;
        entry.attempts = 0;
        entry.last_error = None;
        entry.retry_at = None;
        entry.object_key = Some(uploaded.object_key.clone());
        entry.url = Some(uploaded.url.clone());
        entry.destination_id = Some(uploaded.destination_id.clone());
        entry.handled_at = now.millis;
        entry.reused = uploaded.reused;
        if uploaded.content_hash.is_some() {
            entry.sha256.clone_from(&uploaded.content_hash);
        }
        entry.gone_at = None;
        entry.prior_state = None;
        entry.remote_delete = None;
        self.ledger.put(&entry);

        // Aktar's own moves aren't the user deleting the file: the row is
        // out of the way before the file goes, so nothing marks it gone (and
        // deletes the upload). It's put back if the move fails.
        let mut problem = None;
        match folder.after_upload {
            AfterUpload::Keep | AfterUpload::Tag => {}
            _ if !verified => problem = Some(t!("{0} was uploaded, but Aktar couldn’t confirm it in the bucket, so the original was kept.", uploaded.filename)),
            // A folder on its way was swapped for a junction since the scan.
            _ if !platform::is_inside(&folder.path, &path) => {
                problem = Some(t!("{0} was uploaded, but it’s no longer inside the watched folder, so the original was kept.", uploaded.filename))
            }
            AfterUpload::Trash => {
                self.ledger.remove(folder_id, relative);
                if let Err(error) = platform::move_to_trash(&path) {
                    self.ledger.put(&entry);
                    problem = Some(t!("Couldn’t move {0} to the Recycle Bin. {1}", uploaded.filename, error));
                }
            }
            // Uploaded/ is never looked at, so the row has nothing more to
            // do once the file is in there.
            AfterUpload::MoveToUploaded => {
                self.ledger.remove(folder_id, relative);
                if let Err(error) = platform::move_to_uploaded(&folder.path, &path) {
                    self.ledger.put(&entry);
                    problem = Some(t!("Couldn’t move {0} into the Uploaded folder. {1}", uploaded.filename, error));
                }
            }
        }

        let finished = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
                runtime.in_flight.remove(relative);
                runtime.failed_jobs.remove(relative);
                if let Some(problem) = &problem {
                    runtime.last_error = Some(problem.clone());
                }
            }
            let batch = inner.batches.entry(batch_id.to_string()).or_insert_with(|| Batch {
                folder_id: folder_id.to_string(),
                remaining: 1,
                ..Batch::default()
            });
            batch.links.push(uploaded.link.clone());
            batch.names.push(uploaded.filename.clone());
            batch.remaining = batch.remaining.saturating_sub(1);
            (batch.remaining == 0).then(|| inner.batches.remove(batch_id)).flatten()
        };
        if let Some(problem) = &problem {
            self.host.notify(&folder.name, problem);
        }
        if folder.notifications == Notifications::Each {
            if uploaded.reused {
                self.host.notify(&uploaded.filename, &t!("Already uploaded - copied the existing link"));
            } else {
                self.host.notify(&t!("Uploaded"), &uploaded.filename);
            }
        }
        if let Some(batch) = finished {
            self.finish_batch(&folder, batch);
        }
        self.host.changed();

        let hooks: Vec<Hook> = folder.hooks.iter().filter(|hook| hook.enabled).cloned().collect();
        let payload = serde_json::json!({
            "event": "upload.succeeded",
            "folder": { "id": folder.id, "name": folder.name, "path": folder.path.to_string_lossy() },
            "file": { "path": path.to_string_lossy(), "name": crate::util::last_component(relative), "size": entry.size },
            "upload": { "key": uploaded.object_key, "url": uploaded.url, "destinationID": uploaded.destination_id, "reused": uploaded.reused, "shortUrl": uploaded.short_url },
        });
        Some(AfterSuccess { folder, hooks, payload })
    }

    /// An upload failed. Network and server errors are tried again on their
    /// own, after 1, 5, 15 and then every 60 minutes; anything else waits
    /// for Retry.
    pub fn failed(&self, folder_id: &str, relative: &str, message: String, retryable: bool, now: Now) {
        let Some(folder) = self.store.folder(folder_id) else { return };
        let mut entry = self.ledger.get(folder_id, relative).unwrap_or_else(|| {
            Entry::new(folder_id, relative, FileFacts { size: 0, mtime: 0, file_id: None }, State::Failed)
        });
        entry.state = State::Failed;
        entry.attempts += 1;
        entry.last_error = Some(message.clone());
        entry.handled_at = now.millis;
        entry.retry_at = retryable.then(|| {
            let minutes = RETRY_MINUTES[(entry.attempts as usize).saturating_sub(1).min(RETRY_MINUTES.len() - 1)];
            now.millis + minutes * 60_000
        });
        self.ledger.put(&entry);
        // Said once: a retry that fails again (on its own, or Retry in the
        // panel) stays quiet; the folder's card still shows it.
        let first = entry.attempts == 1;
        let finished = {
            let mut inner = self.inner.lock().unwrap();
            let mut batch_id = None;
            if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
                if let Some(flight) = runtime.in_flight.remove(relative) {
                    if let Some(job_id) = flight.job_id {
                        runtime.failed_jobs.insert(relative.to_string(), job_id);
                    }
                    batch_id = Some(flight.batch_id);
                }
                runtime.last_error = Some(message.clone());
            }
            let name = crate::util::last_component(relative).to_string();
            let failures = if first { vec![(name, message)] } else { Vec::new() };
            match batch_id.and_then(|id| inner.batches.get_mut(&id).map(|batch| (id, batch))) {
                Some((id, batch)) => {
                    batch.failures.extend(failures);
                    batch.remaining = batch.remaining.saturating_sub(1);
                    (batch.remaining == 0).then(|| inner.batches.remove(&id)).flatten()
                }
                None => Some(Batch { folder_id: folder_id.to_string(), failures, ..Batch::default() }),
            }
        };
        if let Some(batch) = finished {
            self.finish_batch(&folder, batch);
        }
        self.host.changed();
    }

    /// Cancelled in the panel: the user doesn't want it uploaded, so it's
    /// skipped from now on.
    pub fn cancelled(&self, folder_id: &str, relative: &str, batch_id: &str) {
        if let Some(mut entry) = self.ledger.get(folder_id, relative) {
            entry.state = State::Skipped;
            entry.retry_at = None;
            self.ledger.put(&entry);
        }
        let finished = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
                runtime.in_flight.remove(relative);
                runtime.failed_jobs.remove(relative);
            }
            match inner.batches.get_mut(batch_id) {
                Some(batch) => {
                    batch.remaining = batch.remaining.saturating_sub(1);
                    (batch.remaining == 0).then(|| inner.batches.remove(batch_id)).flatten()
                }
                None => None,
            }
        };
        if let (Some(batch), Some(folder)) = (finished, self.store.folder(folder_id)) {
            self.finish_batch(&folder, batch);
        }
        self.host.changed();
    }

    /// Once a batch has settled: its links on the clipboard, one
    /// notification for it, and one for what failed.
    fn finish_batch(&self, folder: &WatchedFolder, batch: Batch) {
        if folder.clipboard == ClipboardPolicy::CopyLink && !batch.links.is_empty() {
            self.host.copy(&batch.links);
        }
        if folder.notifications == Notifications::Grouped {
            match batch.names.as_slice() {
                [] => {}
                [name] => self.host.notify(&t!("Uploaded"), name),
                names => self.host.notify(&t!("Uploaded"), &t!("Uploaded {0} files from {1}", names.len(), folder.name)),
            }
        }
        // Failures always say so, whatever the folder's notifications.
        match batch.failures.as_slice() {
            [] => {}
            [(name, message)] => self.host.notify(&t!("Upload failed"), &format!("{name}: {message}")),
            failures => self.host.notify(&t!("Upload failed"), &t!("{0} files from {1} couldn’t be uploaded.", failures.len(), folder.name)),
        }
        let _ = batch.folder_id;
    }

    /// Something went wrong after an upload (a hook): it's the folder's
    /// last error, and always notified.
    pub fn problem(&self, folder_id: &str, message: String) {
        let Some(folder) = self.store.folder(folder_id) else { return };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.runtimes.entry(folder_id.to_string()).or_default().last_error = Some(message.clone());
        }
        self.host.notify(&folder.name, &message);
        self.host.changed();
    }

    // MARK: - Retrying

    fn retry_entry(&self, entry: &Entry, now: Now) {
        let Some(folder) = self.store.folder(&entry.folder_id) else {
            self.ledger.remove(&entry.folder_id, &entry.relative_path);
            return;
        };
        let paused = {
            let inner = self.inner.lock().unwrap();
            self.paused_reason(&self.store.get(), inner.conditions, now).is_some()
        };
        if paused || !folder.enabled {
            return;
        }
        let job = {
            let mut inner = self.inner.lock().unwrap();
            let runtime = inner.runtimes.entry(folder.id.clone()).or_default();
            if runtime.in_flight.contains_key(&entry.relative_path) {
                return;
            }
            runtime.failed_jobs.get(&entry.relative_path).cloned()
        };
        // The failed job is still in the panel: run it again, as Retry there
        // would.
        if let Some(job_id) = job {
            if self.host.retry_job(&job_id) {
                let mut retrying = entry.clone();
                retrying.state = State::Pending;
                retrying.retry_at = None;
                self.ledger.put(&retrying);
                let mut inner = self.inner.lock().unwrap();
                if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                    runtime.failed_jobs.remove(&entry.relative_path);
                    runtime
                        .in_flight
                        .insert(entry.relative_path.clone(), Flight { job_id: Some(job_id), batch_id: crate::util::new_id() });
                }
                return;
            }
        }
        // Otherwise it's queued again, if it's still there to upload.
        let path = folder.path.join(&entry.relative_path);
        match rules::check(&folder, &path, &entry.relative_path) {
            Ok(facts) => {
                let overwrite_key = entry.object_key.clone().filter(|_| folder.modified == Modified::Overwrite);
                let ready = Ready {
                    path,
                    relative_path: entry.relative_path.clone(),
                    facts,
                    sha256: entry.sha256.clone(),
                    overwrite_key,
                    confirmed: true,
                };
                self.enqueue(&folder, vec![ready]);
            }
            Err(Rejection::Missing) => self.ledger.remove(&folder.id, &entry.relative_path),
            Err(_) => {
                let mut skipped = entry.clone();
                skipped.state = State::Skipped;
                skipped.retry_at = None;
                self.ledger.put(&skipped);
            }
        }
    }

    /// "Retry Failed" on a folder.
    pub fn retry_failed(&self, folder_id: &str, now: Now) {
        for entry in self.ledger.failed(folder_id) {
            self.retry_entry(&entry, now);
        }
        self.host.changed();
    }

    /// Back online: what failed on the network goes again right away.
    pub fn network_changed(&self, now: Now) {
        self.conditions_changed();
        for entry in self.ledger.retryable() {
            self.retry_entry(&entry, now);
        }
        self.ledger.retry_deletes_now();
        self.host.changed();
    }

    // MARK: - Deleted files

    /// Deletes from the bucket the uploads of files deleted from their
    /// folder, once the grace period is over. Large deletions wait for the
    /// user; shared and reused objects are never deleted.
    fn process_deletes(&self, list: &WatchedFolders, now: Now, paused: bool) -> bool {
        if paused {
            return false;
        }
        let due = self.ledger.due_deletes(now.millis);
        if due.is_empty() {
            return false;
        }
        let mut by_folder: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for entry in due {
            by_folder.entry(entry.folder_id.clone()).or_default().push(entry);
        }
        let mut changed = false;
        for (folder_id, entries) in by_folder {
            let Some(folder) = list.folders.iter().find(|folder| folder.id == folder_id) else { continue };
            // Turned off since (or the original isn't kept any more).
            if !folder.deletes_remote() {
                for entry in entries {
                    self.ledger.put(&Entry { remote_delete: None, ..entry });
                }
                changed = true;
                continue;
            }
            // Waits while files there are still being looked at: one of them
            // may be this file, renamed.
            let busy = {
                let inner = self.inner.lock().unwrap();
                inner.runtimes.get(&folder.id).is_none_or(|runtime| {
                    !runtime.candidates.is_empty() || !runtime.hashing.is_empty() || !runtime.ready.is_empty() || runtime.health == Some(Status::NotFound)
                })
            };
            if !folder.enabled || busy {
                continue;
            }
            let mut send = Vec::new();
            let mut unconfirmed = Vec::new();
            for entry in entries {
                // Something is at its path again.
                if folder.path.join(&entry.relative_path).exists() {
                    self.ledger.put(&Entry { remote_delete: None, ..entry });
                    continue;
                }
                match entry.remote_delete {
                    Some(RemoteDelete::Confirmed) => send.push(entry),
                    _ => unconfirmed.push(entry),
                }
            }
            // Never asked about: what isn't this file's alone to delete.
            let unconfirmed = self.not_shared(folder, unconfirmed);
            if !unconfirmed.is_empty() {
                let summary = self.ledger.summary(&folder.id);
                let count = unconfirmed.len();
                let large = count > LARGE_BATCH || (count >= 10 && count * 2 > summary.uploaded + count) || summary.held > 0;
                // "Ask before deleting" asks about any number of files.
                if large || folder.confirm_delete {
                    let held = summary.held + count;
                    let question = match unconfirmed.as_slice() {
                        [only] if held == 1 => {
                            t!("{0} was removed from {1}. Delete it from the bucket too?", crate::util::last_component(&only.relative_path), folder.name)
                        }
                        _ => t!("{0} files were removed from {1}. Delete them from the bucket too?", held, folder.name),
                    };
                    for entry in unconfirmed {
                        self.ledger.put(&Entry { remote_delete: Some(RemoteDelete::Held), ..entry });
                    }
                    // Whatever the folder's notifications: nothing goes
                    // without an answer.
                    self.host.notify(&question, &t!("Open Aktar to delete or keep them."));
                } else {
                    send.extend(unconfirmed);
                }
            }
            self.send_deletes(folder, send);
            changed = true;
        }
        changed
    }

    /// The entries whose object is theirs alone to delete; the others stop
    /// waiting to be deleted. Not this file's alone: an earlier upload's
    /// link was reused, or another file or upload still has the object.
    fn not_shared(&self, folder: &WatchedFolder, entries: Vec<Entry>) -> Vec<Entry> {
        let mut own = Vec::new();
        for entry in entries {
            let shared = match (&entry.destination_id, &entry.object_key) {
                (Some(destination_id), Some(object_key)) => {
                    entry.reused
                        || !self.ledger.references(destination_id, object_key).is_empty()
                        || self.host.used_elsewhere(&folder.id, destination_id, object_key)
                }
                _ => true,
            };
            if shared {
                self.ledger.put(&Entry { remote_delete: None, ..entry });
            } else {
                own.push(entry);
            }
        }
        own
    }

    fn send_deletes(&self, folder: &WatchedFolder, entries: Vec<Entry>) {
        let mut sending = Vec::new();
        // Checked again: something may share the object since it was asked.
        for entry in self.not_shared(folder, entries) {
            let running = Entry { remote_delete: Some(RemoteDelete::Running), ..entry };
            self.ledger.put(&running);
            sending.push(running);
        }
        if sending.is_empty() {
            return;
        }
        let batch_id = crate::util::new_id();
        self.inner.lock().unwrap().delete_batches.insert(batch_id.clone(), DeleteBatch { remaining: sending.len(), names: Vec::new() });
        for entry in &sending {
            self.host.delete_remote(folder, entry, &batch_id);
        }
    }

    /// The result of `Host::delete_remote`. `Err((message, transient))`:
    /// network and server errors are tried again with the uploads'
    /// backoff.
    pub fn remote_deleted(&self, folder_id: &str, relative: &str, batch_id: &str, result: Result<(), (String, bool)>, now: Now) {
        let Some(folder) = self.store.folder(folder_id) else { return };
        let name = crate::util::last_component(relative).to_string();
        let entry = self.ledger.get(folder_id, relative).filter(|entry| entry.state == State::Gone);
        match &result {
            Ok(()) => {
                if entry.is_some() {
                    self.ledger.remove(folder_id, relative);
                }
                if folder.notifications == Notifications::Each {
                    self.host.notify(&folder.name, &t!("Deleted {0} from the bucket", name));
                }
            }
            Err((message, transient)) => {
                let mut notify = !transient;
                if let Some(mut entry) = entry {
                    entry.attempts += 1;
                    entry.last_error = Some(message.clone());
                    notify |= entry.attempts == 1;
                    if *transient {
                        let minutes = RETRY_MINUTES[(entry.attempts as usize).saturating_sub(1).min(RETRY_MINUTES.len() - 1)];
                        entry.retry_at = Some(now.millis + minutes * 60_000);
                        entry.remote_delete = Some(RemoteDelete::Confirmed);
                    } else {
                        entry.remote_delete = Some(RemoteDelete::Failed);
                    }
                    self.ledger.put(&entry);
                }
                let mut inner = self.inner.lock().unwrap();
                inner.runtimes.entry(folder_id.to_string()).or_default().last_error = Some(message.clone());
                drop(inner);
                // Failures always say so, once.
                if notify {
                    self.host.notify(&folder.name, &format!("{} {message}", t!("Couldn't delete {0} from the bucket", name)));
                }
            }
        }
        let finished = {
            let mut inner = self.inner.lock().unwrap();
            match inner.delete_batches.get_mut(batch_id) {
                Some(batch) => {
                    if result.is_ok() {
                        batch.names.push(name);
                    }
                    batch.remaining = batch.remaining.saturating_sub(1);
                    (batch.remaining == 0).then(|| inner.delete_batches.remove(batch_id)).flatten()
                }
                None => None,
            }
        };
        if let Some(batch) = finished.filter(|_| folder.notifications == Notifications::Grouped) {
            match batch.names.as_slice() {
                [] => {}
                [name] => self.host.notify(&folder.name, &t!("Deleted {0} from the bucket", name)),
                names => self.host.notify(&folder.name, &t!("Deleted {0} files from the bucket", names.len())),
            }
        }
        self.host.changed();
    }

    /// "Delete from Bucket" (true) or "Keep Uploaded Files" on a large
    /// deletion.
    pub fn confirm_deletions(&self, folder_id: &str, delete: bool) {
        for entry in self.ledger.held_deletes(folder_id) {
            let remote_delete = delete.then_some(RemoteDelete::Confirmed);
            self.ledger.put(&Entry { remote_delete, ..entry });
        }
        self.host.changed();
    }

    // MARK: - The user's choices

    /// "Upload" (or "Skip") on a held large batch.
    pub fn confirm(&self, folder_id: &str, upload: bool) {
        let Some(folder) = self.store.folder(folder_id) else { return };
        let held = {
            let mut inner = self.inner.lock().unwrap();
            inner.runtimes.get_mut(folder_id).map(|runtime| std::mem::take(&mut runtime.awaiting)).unwrap_or_default()
        };
        // Files deleted since they were held are neither uploaded nor skipped.
        let held: Vec<Ready> = held.into_iter().filter(|file| !is_missing(&file.path)).collect();
        if upload {
            self.enqueue(&folder, held);
        } else {
            let skipped: Vec<Entry> = held
                .iter()
                .map(|file| Entry { sha256: file.sha256.clone(), ..Entry::new(folder_id, &file.relative_path, file.facts, State::Skipped) })
                .collect();
            self.ledger.put_all(&skipped);
        }
        self.host.changed();
    }

    /// "Upload Pending Now": a held batch goes up, and waiting files are
    /// looked at again right away.
    pub fn upload_pending_now(&self, folder_id: &str, now: Now) {
        self.confirm(folder_id, true);
        let mut inner = self.inner.lock().unwrap();
        if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
            for candidate in runtime.candidates.values_mut() {
                candidate.stability.next_check = now.at;
            }
            runtime.next_scan = Some(runtime.next_scan.unwrap_or(now.at).min(now.at + SCAN_EVERY));
            runtime.last_ready = runtime.last_ready.map(|_| now.at - BATCH_WINDOW);
        }
    }

    /// Adds a folder. Its files are uploaded now when `upload_existing`, and
    /// otherwise remembered as skipped, so only what arrives from now on
    /// goes up.
    pub fn add(&self, folder: WatchedFolder, upload_existing: bool, now: Now) -> WatchedFolder {
        if !upload_existing {
            self.write_baseline(&folder, now);
        }
        self.store.update(|list| list.folders.push(folder.clone()));
        {
            let mut inner = self.inner.lock().unwrap();
            inner.runtimes.insert(folder.id.clone(), Runtime { path: folder.path.clone(), ..Runtime::default() });
        }
        let paused = {
            let inner = self.inner.lock().unwrap();
            self.paused_reason(&self.store.get(), inner.conditions, now).is_some()
        };
        if !paused {
            self.reconcile(&folder, now, upload_existing);
        }
        self.host.changed();
        folder
    }

    /// Every file in the folder (whatever the filter and subfolder
    /// settings, so changing those later doesn't upload old files) as
    /// skipped.
    fn write_baseline(&self, folder: &WatchedFolder, now: Now) {
        let everything = WatchedFolder { subfolders: super::model::Subfolders::KeepStructure, ..folder.clone() };
        let entries: Vec<Entry> = rules::scan(&everything, None)
            .files
            .into_iter()
            .map(|listed| {
                // The ID tells a renamed old file from a new one later.
                let file_id = platform::probe(&listed.path).ok().and_then(|probe| probe.file_id);
                let facts = FileFacts { size: listed.size, mtime: listed.mtime, file_id };
                Entry { handled_at: now.millis, ..Entry::new(&folder.id, &listed.relative, facts, State::Skipped) }
            })
            .collect();
        self.ledger.put_all(&entries);
    }

    /// Saves an edited folder. A new path starts over: what's in the new
    /// folder is there already, and skipped.
    pub fn update(&self, folder: WatchedFolder, now: Now) {
        let Some(previous) = self.store.folder(&folder.id) else { return };
        if previous.path != folder.path {
            self.ledger.remove_folder(&folder.id);
            self.write_baseline(&folder, now);
        }
        self.store.update(|list| {
            if let Some(existing) = list.folders.iter_mut().find(|existing| existing.id == folder.id) {
                *existing = folder.clone();
            }
        });
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(runtime) = inner.runtimes.get_mut(&folder.id) {
                runtime.candidates.clear();
                runtime.hashing.clear();
                runtime.ready.clear();
                runtime.health = None;
                runtime.next_scan = None;
            }
        }
        self.host.changed();
    }

    pub fn set_enabled(&self, folder_id: &str, enabled: bool) -> Option<WatchedFolder> {
        self.store.update(|list| {
            if let Some(folder) = list.folders.iter_mut().find(|folder| folder.id.eq_ignore_ascii_case(folder_id)) {
                folder.enabled = enabled;
            }
        });
        {
            let mut inner = self.inner.lock().unwrap();
            if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
                runtime.candidates.clear();
                runtime.hashing.clear();
                runtime.ready.clear();
                runtime.next_scan = None;
            }
        }
        self.host.changed();
        self.store.folder(folder_id)
    }

    /// Forgets the folder and everything it handled. Uploads already
    /// queued finish, as uploads of their own.
    pub fn remove(&self, folder_id: &str) {
        self.store.update(|list| list.folders.retain(|folder| folder.id != folder_id));
        self.ledger.remove_folder(folder_id);
        self.inner.lock().unwrap().runtimes.remove(folder_id);
        self.host.changed();
    }

    /// "Reset": forgets what the folder handled, so everything in it counts
    /// as new (behind the large batch prompt).
    pub fn reset(&self, folder_id: &str) {
        self.ledger.remove_folder(folder_id);
        let mut inner = self.inner.lock().unwrap();
        if let Some(runtime) = inner.runtimes.get_mut(folder_id) {
            runtime.awaiting.clear();
            runtime.failed_jobs.clear();
            runtime.last_error = None;
            runtime.next_scan = None;
        }
        drop(inner);
        self.host.changed();
    }

    pub fn set_pause(&self, pause: Pause) {
        self.store.update(|list| list.paused_until = pause);
        self.host.changed();
    }

    pub fn set_pause_conditions(&self, on_battery: Option<bool>, on_metered: Option<bool>) {
        self.store.update(|list| {
            if let Some(value) = on_battery {
                list.pause_on_battery = value;
            }
            if let Some(value) = on_metered {
                list.pause_on_metered = value;
            }
        });
        self.conditions_changed();
        self.host.changed();
    }
}

/// Whether nothing is at `path` any more (not just unreadable).
fn is_missing(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// How many files `folder` has that it would upload, for "This folder
/// already has 37 files".
pub fn existing_files(folder: &WatchedFolder) -> usize {
    rules::scan(folder, None).files.iter().filter(|listed| rules::check_listed(folder, listed)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Now, seconds: u64) -> Now {
        start.after(Duration::from_secs(seconds))
    }

    #[test]
    fn waits_until_a_file_settles() {
        let start = Now::current();
        let mtime = start.millis - 10_000;
        let mut stability = Stability::new(start);
        assert_eq!(stability.next_check, start.at + FIRST_CHECK);
        // First look: nothing to compare with yet.
        assert_eq!(stability.observe(10, mtime, true, at(start, 1)), Verdict::Wait);
        assert_eq!(stability.observe(10, mtime, true, at(start, 2)), Verdict::Ready);

        // Still growing: checked every second.
        let mut growing = Stability::new(start);
        for second in 1..5 {
            assert_eq!(growing.observe(second * 100, start.millis, true, at(start, second)), Verdict::Wait);
            assert_eq!(growing.next_check, at(start, second).at + FIRST_CHECK);
        }

        // Written a moment ago: not yet.
        let mut fresh = Stability::new(start);
        let now = at(start, 1);
        fresh.observe(10, now.millis - 500, true, now);
        assert_eq!(fresh.observe(10, now.millis - 500, true, now.after(Duration::from_millis(250))), Verdict::Wait);
        assert_eq!(fresh.observe(10, now.millis - 500, true, at(start, 4)), Verdict::Ready);

        // Open for writing elsewhere: keeps waiting, backing off to 30 s.
        let mut locked = Stability::new(start);
        let mut delays = Vec::new();
        for second in 1..10 {
            assert_eq!(locked.observe(10, mtime, false, at(start, second)), Verdict::Wait);
            delays.push(locked.delay.as_millis());
        }
        assert_eq!(delays, vec![500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
        assert_eq!(locked.observe(10, mtime, true, at(start, 100)), Verdict::Ready);
    }
}

//! The engine against a real temporary folder, with a fake upload pipeline
//! and a clock the tests move along.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use super::engine::{Conditions, Engine, Host, Now, Planned, Status, Uploaded};
use super::ledger::{Entry, Ledger, RemoteDelete, State};
use super::model::{
    AfterUpload, ClipboardPolicy, Modified, Notifications, OnDelete, Pause, Subfolders, WatchedFolder, WatchedFolderStore, WatchedFolders,
};
use super::rules;

#[derive(Default)]
struct FakeHost {
    queued: Mutex<Vec<Planned>>,
    /// Remote deletes asked for: (relative path, batch ID).
    deleted: Mutex<Vec<(String, String)>>,
    /// Objects the history has from elsewhere.
    elsewhere: Mutex<Vec<String>>,
    notifications: Mutex<Vec<(String, String)>>,
    copied: Mutex<Vec<String>>,
    /// The energy saver is on.
    saver: Mutex<bool>,
}

impl Host for FakeHost {
    fn enqueue(&self, _folder: &WatchedFolder, files: &[Planned]) -> Vec<Option<String>> {
        self.queued.lock().unwrap().extend(files.iter().cloned());
        files.iter().map(|file| Some(format!("job-{}", file.relative_path))).collect()
    }

    fn retry_job(&self, _job_id: &str) -> bool {
        false
    }

    fn notify(&self, title: &str, body: &str) {
        self.notifications.lock().unwrap().push((title.to_string(), body.to_string()));
    }

    fn copy(&self, links: &[String]) {
        self.copied.lock().unwrap().push(links.join("\n"));
    }

    fn changed(&self) {}

    fn conditions(&self) -> Conditions {
        Conditions::default()
    }

    fn delete_remote(&self, _folder: &WatchedFolder, entry: &Entry, batch_id: &str) {
        self.deleted.lock().unwrap().push((entry.relative_path.clone(), batch_id.to_string()));
    }

    fn used_elsewhere(&self, _folder_id: &str, _destination_id: &str, object_key: &str) -> bool {
        self.elsewhere.lock().unwrap().iter().any(|key| key == object_key)
    }

    fn energy_saver(&self) -> bool {
        *self.saver.lock().unwrap()
    }
}

struct Setup {
    engine: Engine<FakeHost>,
    root: PathBuf,
    dir: PathBuf,
    now: Now,
}

impl Setup {
    fn new(change: impl FnOnce(&mut WatchedFolder)) -> Self {
        Self::hashing(change, std::sync::Arc::new(|path| crate::util::sha256_file(path).ok()))
    }

    /// With its own hasher, to count or slow down hashing.
    fn hashing(change: impl FnOnce(&mut WatchedFolder), hasher: super::engine::Hasher) -> Self {
        crate::i18n::apply(Some("en"));
        let root = std::env::temp_dir().join(format!("aktar-watched-{}", crate::util::new_id()));
        let dir = root.join("Drop");
        std::fs::create_dir_all(&dir).unwrap();
        let engine = Engine::new(FakeHost::default(), WatchedFolderStore::in_memory(WatchedFolders::default()), Ledger::in_memory())
            .with_hasher(hasher);
        let mut folder = WatchedFolder::new(&dir);
        change(&mut folder);
        let now = Now::current();
        engine.add(folder, false, now);
        Self { engine, root, dir, now }
    }

    fn folder(&self) -> WatchedFolder {
        self.engine.store.get().folders[0].clone()
    }

    fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// A hint for the file, as the watcher would send.
    fn hint(&self, path: &Path) {
        self.engine.hint(&self.folder().id, Some(path.to_path_buf()));
    }

    /// Moves time along by `seconds`, ticking twice a second.
    fn run(&mut self, seconds: u64) {
        for _ in 0..seconds * 2 {
            self.tick();
        }
    }

    /// One half-second step of the clock, once the hashes started by the
    /// last step are back (they're applied by this one).
    fn tick(&mut self) {
        self.engine.wait_for_hashes();
        self.now = self.now.after(Duration::from_millis(500));
        self.engine.tick(self.now);
    }

    fn queued(&self) -> Vec<String> {
        self.engine.host.queued.lock().unwrap().iter().map(|planned| planned.relative_path.clone()).collect()
    }

    fn status(&self) -> super::engine::FolderInfo {
        self.engine.overview(self.now).folders.remove(0)
    }

    /// What the pipeline reports when `planned` went up.
    fn succeed(&self, planned: &Planned, key: &str) {
        let uploaded = Uploaded {
            object_key: key.to_string(),
            url: format!("https://cdn.example.com/{key}"),
            link: format!("https://cdn.example.com/{key}"),
            filename: crate::util::last_component(&planned.relative_path).to_string(),
            destination_id: "D".into(),
            reused: false,
            byte_size: 3,
            content_hash: None,
            short_url: None,
        };
        self.engine.succeeded(&planned.folder_id, &planned.relative_path, &planned.batch_id, &uploaded, true, self.now);
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn holds_a_large_batch_for_confirmation() {
    let mut setup = Setup::new(|_| {});
    for index in 0..60 {
        let path = setup.write(&format!("shot-{index:02}.png"), b"png");
        setup.hint(&path);
    }
    setup.run(8);
    assert!(setup.queued().is_empty());
    let status = setup.status();
    assert_eq!(status.awaiting_confirmation, 60);
    assert!(setup.engine.host.notifications.lock().unwrap().iter().any(|(title, _)| title == "60 new files in Drop"));

    setup.engine.confirm(&setup.folder().id, true);
    assert_eq!(setup.queued().len(), 60);
    assert_eq!(setup.status().uploading, 60);
    assert_eq!(setup.status().awaiting_confirmation, 0);
}

#[test]
fn deleting_a_held_batch_withdraws_the_ask() {
    let mut setup = Setup::new(|_| {});
    let paths: Vec<_> = (0..60).map(|index| setup.write(&format!("shot-{index:02}.png"), b"png")).collect();
    paths.iter().for_each(|path| setup.hint(path));
    setup.run(8);
    assert_eq!(setup.status().awaiting_confirmation, 60);
    for path in &paths {
        std::fs::remove_file(path).unwrap();
        setup.hint(path);
    }
    setup.run(8);
    assert_eq!(setup.status().awaiting_confirmation, 0);
    // The folder isn't stuck behind the old ask: a new file goes up.
    let path = setup.write("after.png", b"after");
    setup.hint(&path);
    setup.run(8);
    assert_eq!(setup.queued(), vec!["after.png".to_string()]);
}

#[test]
fn uploading_a_held_batch_leaves_out_deleted_files() {
    let mut setup = Setup::new(|_| {});
    let paths: Vec<_> = (0..60).map(|index| setup.write(&format!("shot-{index:02}.png"), b"png")).collect();
    paths.iter().for_each(|path| setup.hint(path));
    setup.run(8);
    // Deleted without Aktar hearing about it.
    paths[..10].iter().for_each(|path| std::fs::remove_file(path).unwrap());
    setup.engine.confirm(&setup.folder().id, true);
    assert_eq!(setup.queued().len(), 50);
    assert!(!setup.queued().contains(&"shot-00.png".to_string()));
}

#[test]
fn skipping_a_large_batch_remembers_the_files() {
    let mut setup = Setup::new(|_| {});
    for index in 0..51 {
        setup.write(&format!("{index}.txt"), b"txt");
    }
    setup.engine.hint(&setup.folder().id, None);
    setup.run(8);
    assert_eq!(setup.status().awaiting_confirmation, 51);
    setup.engine.confirm(&setup.folder().id, false);
    assert_eq!(setup.engine.ledger.get(&setup.folder().id, "7.txt").unwrap().state, State::Skipped);
    setup.engine.hint(&setup.folder().id, None);
    setup.run(8);
    assert!(setup.queued().is_empty());
    assert_eq!(setup.status().awaiting_confirmation, 0);
}

#[test]
fn uploads_a_download_once_its_renamed_to_its_final_name() {
    let mut setup = Setup::new(|_| {});
    let partial = setup.write("movie.mp4.crdownload", b"partial");
    setup.hint(&partial);
    setup.run(5);
    assert!(setup.queued().is_empty());
    let done = setup.dir.join("movie.mp4");
    std::fs::rename(&partial, &done).unwrap();
    setup.hint(&partial);
    setup.hint(&done);
    setup.run(6);
    assert_eq!(setup.queued(), vec!["movie.mp4".to_string()]);
}

#[test]
fn waits_for_a_growing_file_to_settle() {
    use std::io::Write;
    let mut setup = Setup::new(|_| {});
    let path = setup.write("export.csv", b"a");
    setup.hint(&path);
    for _ in 0..6 {
        setup.tick();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"more").unwrap();
        // Written just now, as far as the moved clock goes.
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_millis(setup.now.millis as u64)).unwrap();
        assert!(setup.queued().is_empty(), "uploaded while it was still being written");
        assert_eq!(setup.status().waiting, 1);
    }
    setup.run(10);
    assert_eq!(setup.queued(), vec!["export.csv".to_string()]);
    let entry = setup.engine.ledger.get(&setup.folder().id, "export.csv").unwrap();
    assert_eq!(entry.size, 1 + 6 * 4);
    assert_eq!(entry.state, State::Pending);
}

#[test]
fn picks_up_files_that_arrived_while_paused() {
    let mut setup = Setup::new(|_| {});
    setup.engine.set_pause(Pause::Forever);
    setup.run(1);
    let path = setup.write("while-paused.png", b"png");
    setup.hint(&path);
    setup.run(6);
    assert!(setup.queued().is_empty());
    assert_eq!(setup.status().status, Status::Paused);

    setup.engine.set_pause(Pause::None);
    setup.run(6);
    assert_eq!(setup.queued(), vec!["while-paused.png".to_string()]);
    assert_eq!(setup.status().status, Status::Watching);
}

#[test]
fn a_paused_folder_waits_for_its_switch() {
    let mut setup = Setup::new(|_| {});
    let id = setup.folder().id;
    setup.engine.set_enabled(&id, false);
    let path = setup.write("a.png", b"png");
    setup.hint(&path);
    setup.run(6);
    assert!(setup.queued().is_empty());
    assert_eq!(setup.status().status, Status::Disabled);
    setup.engine.set_enabled(&id, true);
    setup.run(6);
    assert_eq!(setup.queued(), vec!["a.png".to_string()]);
}

#[test]
fn survives_its_folder_being_deleted_and_recreated() {
    let mut setup = Setup::new(|_| {});
    std::fs::remove_dir_all(&setup.dir).unwrap();
    setup.engine.hint(&setup.folder().id, None);
    setup.run(2);
    assert_eq!(setup.status().status, Status::NotFound);
    // Its parent is watched instead, to notice it coming back.
    let plan = setup.engine.watch_plan(setup.now);
    assert_eq!(plan.len(), 1);
    assert!(plan[0].parent);
    assert_eq!(plan[0].path, setup.root);

    std::fs::create_dir_all(&setup.dir).unwrap();
    setup.write("back.png", b"png");
    // The parent's event.
    setup.engine.hint(&setup.folder().id, None);
    setup.run(5);
    assert_eq!(setup.status().status, Status::Watching);
    let plan = setup.engine.watch_plan(setup.now);
    assert!(!plan[0].parent);
    assert_eq!(plan[0].path, setup.dir);
    assert_eq!(setup.queued(), vec!["back.png".to_string()]);

    // Without an event, it's noticed by the fallback check.
    std::fs::remove_dir_all(&setup.dir).unwrap();
    setup.engine.hint(&setup.folder().id, None);
    setup.run(2);
    assert_eq!(setup.status().status, Status::NotFound);
    std::fs::create_dir_all(&setup.dir).unwrap();
    setup.run(60);
    assert_eq!(setup.status().status, Status::NotFound);
    setup.run(65);
    assert_eq!(setup.status().status, Status::Watching);
}

#[test]
fn never_uploads_the_uploaded_folder() {
    let mut setup = Setup::new(|folder| {
        folder.after_upload = AfterUpload::MoveToUploaded;
        folder.subfolders = Subfolders::KeepStructure;
        folder.clipboard = ClipboardPolicy::CopyLink;
    });
    let path = setup.write("report.pdf", b"pdf");
    setup.hint(&path);
    setup.run(6);
    let planned = setup.engine.host.queued.lock().unwrap()[0].clone();
    setup.succeed(&planned, "2026/report.pdf");
    assert!(!path.exists());
    let moved = setup.dir.join("Uploaded/report.pdf");
    assert!(moved.exists());
    assert_eq!(setup.engine.host.copied.lock().unwrap().as_slice(), ["https://cdn.example.com/2026/report.pdf"]);

    setup.hint(&moved);
    setup.engine.hint(&setup.folder().id, None);
    setup.run(8);
    assert_eq!(setup.queued().len(), 1);
    // Its row went before it did, so it isn't taken for deleted, and
    // Uploaded/ is never looked at.
    assert!(setup.engine.ledger.get(&setup.folder().id, "report.pdf").is_none());
    assert!(setup.engine.ledger.get(&setup.folder().id, "Uploaded/report.pdf").is_none());
}

#[test]
fn skips_whats_there_when_added() {
    let root = std::env::temp_dir().join(format!("aktar-watched-{}", crate::util::new_id()));
    let dir = root.join("Existing");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("old.png"), b"png").unwrap();
    std::fs::write(dir.join("sub/old.png"), b"png").unwrap();
    let engine = Engine::new(FakeHost::default(), WatchedFolderStore::in_memory(WatchedFolders::default()), Ledger::in_memory());
    let draft = WatchedFolder::new(&dir);
    assert_eq!(super::engine::existing_files(&draft), 1);
    let mut now = Now::current();
    let folder = engine.add(draft, false, now);
    for _ in 0..12 {
        now = now.after(Duration::from_millis(500));
        engine.tick(now);
    }
    assert!(engine.host.queued.lock().unwrap().is_empty());
    // Changing the subfolders setting later doesn't upload old files.
    let mut changed = folder.clone();
    changed.subfolders = Subfolders::KeepStructure;
    engine.update(changed, now);
    for _ in 0..12 {
        now = now.after(Duration::from_millis(500));
        engine.tick(now);
    }
    assert!(engine.host.queued.lock().unwrap().is_empty());

    // "Upload Them" does upload them, without asking again.
    let other = root.join("Upload");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("a.png"), b"png").unwrap();
    engine.add(WatchedFolder::new(&other), true, now);
    for _ in 0..12 {
        now = now.after(Duration::from_millis(500));
        engine.tick(now);
    }
    assert_eq!(engine.host.queued.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn follows_changes_as_the_folder_says() {
    let mut setup = Setup::new(|folder| folder.modified = Modified::Overwrite);
    let path = setup.write("notes.md", b"one");
    setup.hint(&path);
    setup.run(6);
    let planned = setup.engine.host.queued.lock().unwrap()[0].clone();
    assert_eq!(planned.overwrite_key, None);
    setup.succeed(&planned, "docs/notes.md");

    // Saved again unchanged: nothing to do.
    let entry = setup.engine.ledger.get(&setup.folder().id, "notes.md").unwrap();
    assert!(entry.sha256.is_some());
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() - Duration::from_secs(60)).unwrap();
    drop(file);
    setup.hint(&path);
    setup.run(6);
    assert_eq!(setup.queued().len(), 1);

    // Changed: goes to the same key again.
    std::fs::write(&path, b"two!").unwrap();
    setup.hint(&path);
    setup.run(6);
    let queued = setup.engine.host.queued.lock().unwrap().clone();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[1].overwrite_key.as_deref(), Some("docs/notes.md"));
}

#[test]
fn follows_a_renamed_file_without_uploading_it() {
    let mut setup = Setup::new(|_| {});
    let path = setup.write("IMG_1.png", b"png");
    setup.hint(&path);
    setup.run(6);
    let planned = setup.engine.host.queued.lock().unwrap()[0].clone();
    setup.succeed(&planned, "IMG_1.png");
    let renamed = setup.dir.join("holiday.png");
    std::fs::rename(&path, &renamed).unwrap();
    setup.hint(&renamed);
    setup.run(6);
    assert_eq!(setup.queued().len(), 1);
    let id = setup.folder().id;
    assert!(setup.engine.ledger.get(&id, "IMG_1.png").is_none());
    assert_eq!(setup.engine.ledger.get(&id, "holiday.png").unwrap().state, State::Uploaded);
}

#[test]
fn retries_network_failures_on_its_own() {
    let mut setup = Setup::new(|folder| folder.notifications = Notifications::FailuresOnly);
    let path = setup.write("a.png", b"png");
    setup.hint(&path);
    setup.run(6);
    let planned = setup.engine.host.queued.lock().unwrap()[0].clone();
    setup.engine.failed(&planned.folder_id, &planned.relative_path, "offline".into(), true, setup.now);
    assert_eq!(setup.status().failed, 1);
    assert!(setup.engine.host.notifications.lock().unwrap().iter().any(|(title, body)| title == "Upload failed" && body == "a.png: offline"));
    setup.run(30);
    assert_eq!(setup.queued().len(), 1);
    // A minute later it goes again (its job is gone, so it's queued anew).
    setup.run(40);
    assert_eq!(setup.queued().len(), 2);

    // Anything else waits for Retry.
    let planned = setup.engine.host.queued.lock().unwrap()[1].clone();
    setup.engine.failed(&planned.folder_id, &planned.relative_path, "denied".into(), false, setup.now);
    setup.run(120);
    assert_eq!(setup.queued().len(), 2);
    setup.engine.retry_failed(&planned.folder_id, setup.now);
    assert_eq!(setup.queued().len(), 3);
}

#[test]
fn refuses_overlapping_folders() {
    let setup = Setup::new(|_| {});
    let watched: Vec<(PathBuf, String)> = setup.engine.store.get().folders.into_iter().map(|folder| (folder.path, folder.name)).collect();
    let protected = rules::Protected { home: None, system: Vec::new(), app: Vec::new() };
    assert!(matches!(rules::forbidden(&setup.dir.join("Inside"), &protected, &watched), Some(rules::Forbidden::Overlaps(_))));
    assert!(matches!(rules::forbidden(&setup.root, &protected, &watched), Some(rules::Forbidden::Overlaps(_))));
    assert!(rules::forbidden(&setup.root.join("Elsewhere"), &protected, &watched).is_none());
}

#[test]
fn groups_notifications_and_links_per_batch() {
    let mut setup = Setup::new(|folder| folder.clipboard = ClipboardPolicy::CopyLink);
    for name in ["a.png", "b.png", "c.png"] {
        let path = setup.write(name, b"png");
        setup.hint(&path);
    }
    setup.run(6);
    let queued = setup.engine.host.queued.lock().unwrap().clone();
    assert_eq!(queued.len(), 3);
    assert!(queued.iter().all(|planned| planned.batch_id == queued[0].batch_id));
    for planned in &queued {
        setup.succeed(planned, &planned.relative_path);
    }
    let notifications = setup.engine.host.notifications.lock().unwrap().clone();
    assert_eq!(notifications, vec![("Uploaded".to_string(), "Uploaded 3 files from Drop".to_string())]);
    assert_eq!(
        setup.engine.host.copied.lock().unwrap().as_slice(),
        ["https://cdn.example.com/a.png\nhttps://cdn.example.com/b.png\nhttps://cdn.example.com/c.png"]
    );
}

// MARK: - Deleted files

impl Setup {
    /// Uploads `name` all the way: written, queued, and reported done.
    fn uploaded(&mut self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.write(name, contents);
        self.hint(&path);
        self.run(6);
        let planned = self.engine.host.queued.lock().unwrap().iter().rev().find(|planned| planned.relative_path == name).unwrap().clone();
        self.succeed(&planned, &format!("k/{name}"));
        path
    }

    fn entry(&self, name: &str) -> Option<Entry> {
        self.engine.ledger.get(&self.folder().id, name)
    }

    fn deleted(&self) -> Vec<String> {
        self.engine.host.deleted.lock().unwrap().iter().map(|(relative, _)| relative.clone()).collect()
    }
}

/// Deletes from the bucket on its own ("Ask before deleting" off).
fn deleting(folder: &mut WatchedFolder) {
    folder.on_delete = OnDelete::DeleteRemote;
    folder.confirm_delete = false;
}

fn asking(folder: &mut WatchedFolder) {
    folder.on_delete = OnDelete::DeleteRemote;
}

#[test]
fn uploads_a_new_file_where_a_deleted_one_was() {
    let mut setup = Setup::new(|_| {});
    let path = setup.uploaded("Screenshot (5).png", b"first");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(3);
    assert_eq!(setup.entry("Screenshot (5).png").unwrap().state, State::Gone);
    // Nothing to delete from the bucket by default.
    assert_eq!(setup.entry("Screenshot (5).png").unwrap().remote_delete, None);
    setup.run(10);
    std::fs::write(&path, b"a different screenshot").unwrap();
    setup.hint(&path);
    setup.run(6);
    assert_eq!(setup.queued().iter().filter(|name| *name == "Screenshot (5).png").count(), 2);
}

#[test]
fn a_file_moved_away_and_back_isnt_uploaded_again() {
    let mut setup = Setup::new(|_| {});
    let path = setup.uploaded("IMG_1.png", b"png");
    let outside = setup.root.join("IMG_1.png");
    std::fs::rename(&path, &outside).unwrap();
    setup.hint(&path);
    setup.run(15);
    assert_eq!(setup.entry("IMG_1.png").unwrap().state, State::Gone);
    let back = setup.dir.join("holiday.png");
    std::fs::rename(&outside, &back).unwrap();
    setup.hint(&back);
    setup.run(6);
    assert_eq!(setup.queued().len(), 1);
    assert!(setup.entry("IMG_1.png").is_none());
    assert_eq!(setup.entry("holiday.png").unwrap().state, State::Uploaded);
}

#[test]
fn an_atomic_save_doesnt_delete_the_upload() {
    let mut setup = Setup::new(deleting);
    let path = setup.uploaded("report.pdf", b"v1");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(2);
    assert_eq!(setup.entry("report.pdf").unwrap().remote_delete, Some(RemoteDelete::Pending));
    std::fs::write(&path, b"v2, saved through a temporary file").unwrap();
    setup.hint(&path);
    setup.run(15);
    assert!(setup.deleted().is_empty());
    // Changes are ignored by default, so that's all.
    assert_eq!(setup.queued().len(), 1);
    let entry = setup.entry("report.pdf").unwrap();
    assert_eq!(entry.state, State::Uploaded);
    assert_eq!(entry.remote_delete, None);
}

#[test]
fn deletes_the_upload_of_a_deleted_file() {
    let mut setup = Setup::new(deleting);
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(2);
    // Not before the grace period is over.
    assert!(setup.deleted().is_empty());
    setup.run(10);
    assert_eq!(setup.deleted(), vec!["a.png".to_string()]);
    assert_eq!(setup.entry("a.png").unwrap().remote_delete, Some(RemoteDelete::Running));
    let batch = setup.engine.host.deleted.lock().unwrap()[0].1.clone();
    setup.engine.remote_deleted(&setup.folder().id, "a.png", &batch, Ok(()), setup.now);
    assert!(setup.entry("a.png").is_none());
    assert!(setup.engine.host.notifications.lock().unwrap().iter().any(|(_, body)| body == "Deleted a.png from the bucket"));
}

#[test]
fn retries_a_delete_that_failed_on_the_network() {
    let mut setup = Setup::new(deleting);
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(15);
    let batch = setup.engine.host.deleted.lock().unwrap()[0].1.clone();
    setup.engine.remote_deleted(&setup.folder().id, "a.png", &batch, Err(("offline".into(), true)), setup.now);
    assert!(setup.engine.host.notifications.lock().unwrap().iter().any(|(_, body)| body.starts_with("Couldn't delete a.png from the bucket")));
    setup.run(30);
    assert_eq!(setup.deleted().len(), 1);
    setup.run(40);
    assert_eq!(setup.deleted().len(), 2);
}

#[test]
fn never_marks_files_gone_while_the_folder_is_missing() {
    let mut setup = Setup::new(deleting);
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_dir_all(&setup.dir).unwrap();
    setup.hint(&path);
    setup.engine.hint(&setup.folder().id, None);
    setup.run(20);
    assert_eq!(setup.status().status, Status::NotFound);
    assert_eq!(setup.entry("a.png").unwrap().state, State::Uploaded);
    assert!(setup.deleted().is_empty());
}

#[test]
fn holds_a_large_deletion_for_confirmation() {
    let mut setup = Setup::new(deleting);
    let id = setup.folder().id;
    let mut paths = Vec::new();
    for index in 0..60 {
        let name = format!("{index:02}.png");
        let path = setup.write(&name, b"png");
        let facts = rules::check(&setup.folder(), &path, &name).unwrap();
        setup.engine.ledger.put(&Entry {
            object_key: Some(format!("k/{name}")),
            destination_id: Some("D".into()),
            ..Entry::new(&id, &name, facts, State::Uploaded)
        });
        paths.push(path);
    }
    for path in &paths {
        std::fs::remove_file(path).unwrap();
    }
    setup.engine.hint(&id, None);
    setup.run(15);
    assert!(setup.deleted().is_empty());
    assert_eq!(setup.status().awaiting_delete_confirmation, 60);
    assert!(setup
        .engine
        .host
        .notifications
        .lock()
        .unwrap()
        .iter()
        .any(|(title, body)| title == "60 files were removed from Drop. Delete them from the bucket too?"
            && body == "Open Aktar to delete or keep them."));
    setup.engine.confirm_deletions(&id, true);
    setup.run(1);
    assert_eq!(setup.deleted().len(), 60);

    // "Keep Uploaded Files" just leaves them gone.
    let mut kept = Setup::new(deleting);
    let kept_id = kept.folder().id;
    for index in 0..12 {
        let name = format!("{index}.png");
        kept.engine.ledger.put(&Entry {
            object_key: Some(format!("k/{name}")),
            destination_id: Some("D".into()),
            ..Entry::new(&kept_id, &name, rules::FileFacts { size: 1, mtime: 1, file_id: None }, State::Uploaded)
        });
    }
    kept.engine.hint(&kept_id, None);
    kept.run(15);
    // 12 of 12 uploads: more than half of them.
    assert_eq!(kept.status().awaiting_delete_confirmation, 12);
    kept.engine.confirm_deletions(&kept_id, false);
    kept.run(15);
    assert!(kept.deleted().is_empty());
    assert_eq!(kept.engine.ledger.entries(&kept_id).iter().filter(|entry| entry.state == State::Gone).count(), 12);
}

#[test]
fn never_deletes_shared_or_reused_objects() {
    let mut setup = Setup::new(deleting);
    let id = setup.folder().id;
    let put = |name: &str, key: &str, reused: bool| {
        let path = setup.write(name, b"png");
        let facts = rules::check(&setup.folder(), &path, name).unwrap();
        setup.engine.ledger.put(&Entry {
            object_key: Some(key.into()),
            destination_id: Some("D".into()),
            reused,
            ..Entry::new(&id, name, facts, State::Uploaded)
        });
        path
    };
    let reused = put("reused.png", "k/earlier.png", true);
    let shared = put("shared.png", "k/same.png", false);
    put("still-here.png", "k/same.png", false);
    let elsewhere = put("elsewhere.png", "k/elsewhere.png", false);
    let own = put("own.png", "k/own.png", false);
    setup.engine.host.elsewhere.lock().unwrap().push("k/elsewhere.png".into());
    for path in [&reused, &shared, &elsewhere, &own] {
        std::fs::remove_file(path).unwrap();
    }
    setup.engine.hint(&id, None);
    setup.run(15);
    assert_eq!(setup.deleted(), vec!["own.png".to_string()]);
    assert_eq!(setup.entry("shared.png").unwrap().state, State::Gone);
    assert_eq!(setup.entry("shared.png").unwrap().remote_delete, None);
}

#[test]
fn aktars_own_moves_never_delete_uploads() {
    // The user deleting a file from a folder that moves originals away
    // doesn't delete anything: the option only works while they stay.
    let mut trash = Setup::new(|folder| {
        deleting(folder);
        folder.after_upload = AfterUpload::Trash;
    });
    let path = trash.write("a.png", b"png");
    trash.hint(&path);
    trash.run(6);
    let planned = trash.engine.host.queued.lock().unwrap()[0].clone();
    let uploaded = Uploaded {
        object_key: "k/a.png".into(),
        url: String::new(),
        link: String::new(),
        filename: "a.png".into(),
        destination_id: "D".into(),
        reused: false,
        byte_size: 3,
        content_hash: None,
        short_url: None,
    };
    // Not confirmed in the bucket: the original stays, for this test.
    trash.engine.succeeded(&planned.folder_id, "a.png", &planned.batch_id, &uploaded, false, trash.now);
    std::fs::remove_file(&path).unwrap();
    trash.hint(&path);
    trash.run(15);
    assert_eq!(trash.entry("a.png").unwrap().state, State::Gone);
    assert!(trash.deleted().is_empty());

    // Moved into Uploaded/ by Aktar: the row follows it, nothing is gone.
    let mut moved = Setup::new(|folder| {
        deleting(folder);
        folder.after_upload = AfterUpload::MoveToUploaded;
    });
    let path = moved.uploaded("b.png", b"png");
    assert!(!path.exists());
    moved.hint(&path);
    moved.engine.hint(&moved.folder().id, None);
    moved.run(15);
    assert!(moved.entry("b.png").is_none());
    assert!(moved.entry("Uploaded/b.png").is_none());
    assert!(moved.deleted().is_empty());
}

#[test]
fn forgets_deleted_files_after_a_day() {
    let mut setup = Setup::new(|_| {});
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(3);
    assert_eq!(setup.entry("a.png").unwrap().state, State::Gone);
    setup.engine.tick(setup.now.after(Duration::from_secs(23 * 3600)));
    assert!(setup.entry("a.png").is_some());
    setup.engine.tick(setup.now.after(Duration::from_secs(25 * 3600)));
    assert!(setup.entry("a.png").is_none());
}

#[test]
fn forgets_a_failed_file_thats_deleted_and_waits_while_paused() {
    let mut setup = Setup::new(deleting);
    let path = setup.write("a.png", b"png");
    setup.hint(&path);
    setup.run(6);
    let planned = setup.engine.host.queued.lock().unwrap()[0].clone();
    setup.engine.failed(&planned.folder_id, "a.png", "denied".into(), false, setup.now);
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(3);
    assert!(setup.entry("a.png").is_none());

    let kept = setup.uploaded("b.png", b"png");
    setup.engine.set_pause(Pause::Forever);
    setup.run(1);
    std::fs::remove_file(&kept).unwrap();
    setup.hint(&kept);
    setup.engine.hint(&setup.folder().id, None);
    setup.run(20);
    assert_eq!(setup.entry("b.png").unwrap().state, State::Uploaded);
    // The scan on resume catches up.
    setup.engine.set_pause(Pause::None);
    setup.run(3);
    assert_eq!(setup.entry("b.png").unwrap().state, State::Gone);
}

#[test]
fn asks_before_deleting_even_one_file() {
    let mut setup = Setup::new(|folder| {
        asking(folder);
        folder.notifications = Notifications::FailuresOnly;
    });
    assert!(setup.folder().confirm_delete);
    let id = setup.folder().id;
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(15);
    assert!(setup.deleted().is_empty());
    let status = setup.status();
    assert_eq!(status.awaiting_delete_confirmation, 1);
    assert_eq!(status.awaiting_delete_name.as_deref(), Some("a.png"));
    // Asked whatever the notifications policy.
    assert!(setup
        .engine
        .host
        .notifications
        .lock()
        .unwrap()
        .iter()
        .any(|(title, _)| title == "a.png was removed from Drop. Delete it from the bucket too?"));
    // It never goes on its own.
    setup.engine.tick(setup.now.after(Duration::from_secs(3 * 24 * 3600)));
    assert!(setup.deleted().is_empty());
    assert_eq!(setup.entry("a.png").unwrap().remote_delete, Some(RemoteDelete::Held));
    setup.engine.confirm_deletions(&id, true);
    setup.run(1);
    assert_eq!(setup.deleted(), vec!["a.png".to_string()]);

    // "Keep Uploaded Files".
    let path = setup.uploaded("b.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(15);
    assert_eq!(setup.status().awaiting_delete_confirmation, 1);
    setup.engine.confirm_deletions(&id, false);
    setup.run(15);
    assert_eq!(setup.deleted().len(), 1);
    assert_eq!(setup.entry("b.png").unwrap().remote_delete, None);
}

#[test]
fn a_file_coming_back_withdraws_the_ask() {
    let mut setup = Setup::new(asking);
    let path = setup.uploaded("a.png", b"png");
    std::fs::remove_file(&path).unwrap();
    setup.hint(&path);
    setup.run(15);
    assert_eq!(setup.status().awaiting_delete_confirmation, 1);
    std::fs::write(&path, b"png").unwrap();
    setup.hint(&path);
    setup.run(6);
    assert_eq!(setup.status().awaiting_delete_confirmation, 0);
    assert_ne!(setup.entry("a.png").unwrap().remote_delete, Some(RemoteDelete::Held));
    assert!(setup.deleted().is_empty());
}

// MARK: - Performance

#[test]
fn schedules_nothing_without_folders() {
    let engine = Engine::new(FakeHost::default(), WatchedFolderStore::in_memory(WatchedFolders::default()), Ledger::in_memory());
    let now = Now::current();
    assert_eq!(engine.tick(now), None);
    // Nor once the last one is removed.
    let setup = Setup::new(|_| {});
    let id = setup.folder().id;
    setup.engine.remove(&id);
    assert_eq!(setup.engine.tick(setup.now.after(Duration::from_secs(1))), None);
}

#[test]
fn an_idle_folder_only_wakes_for_its_hourly_scan() {
    let mut setup = Setup::new(|_| {});
    setup.uploaded("a.png", b"png");
    setup.run(5);
    let now = setup.now.after(Duration::from_millis(500));
    let next = setup.engine.tick(now).expect("the safety scan");
    let wait = next.duration_since(now.at);
    assert!(wait >= Duration::from_secs(60 * 60) && wait <= Duration::from_secs(70 * 60), "{wait:?}");
    // Paused: nothing at all until it's resumed.
    setup.engine.set_pause(Pause::Forever);
    assert_eq!(setup.engine.tick(now.after(Duration::from_secs(1))), None);
    // Paused for an hour: the end of the pause.
    setup.engine.set_pause(Pause::Until(now.millis + 30 * 60_000));
    let next = setup.engine.tick(now.after(Duration::from_secs(2))).unwrap();
    assert!(next.duration_since(now.at) <= Duration::from_secs(30 * 60));
    // A pause date from a file can be anything: no overflow, at most a
    // year and a day of waiting.
    setup.engine.set_pause(Pause::Until(i64::MAX));
    let next = setup.engine.tick(now.after(Duration::from_secs(3))).unwrap();
    assert!(next.duration_since(now.at) <= Duration::from_secs(367 * 24 * 60 * 60));
}

#[test]
fn clamps_pause_minutes() {
    assert_eq!(super::pause_for(Some(60), 1_000), Pause::Until(1_000 + 60 * 60_000));
    assert_eq!(super::pause_for(Some(u64::MAX), 1_000), Pause::Until(1_000 + super::MAX_PAUSE_MINUTES as i64 * 60_000));
    assert_eq!(super::pause_for(Some(10), i64::MAX - 5), Pause::Until(i64::MAX));
    assert_eq!(super::pause_for(None, 0), Pause::Forever);
    assert_eq!(super::pause_for(Some(0), 0), Pause::Forever);

    assert_eq!(super::pause_minutes(&serde_json::json!(30)), Some(30));
    assert_eq!(super::pause_minutes(&serde_json::json!(9_999_999)), Some(super::MAX_PAUSE_MINUTES));
    for invalid in [serde_json::json!(0), serde_json::json!(-5), serde_json::json!(1.5), serde_json::json!("60"), serde_json::json!(true)] {
        assert_eq!(super::pause_minutes(&invalid), None, "{invalid}");
    }
    assert_eq!(super::pause_minutes_text(" 15 "), Some(15));
    assert_eq!(super::pause_minutes_text("99999999999999999999"), Some(super::MAX_PAUSE_MINUTES));
    assert_eq!(super::pause_minutes_text("600000"), Some(super::MAX_PAUSE_MINUTES));
    for invalid in ["", "0", "-1", "soon", "1.5"] {
        assert_eq!(super::pause_minutes_text(invalid), None, "{invalid}");
    }
}

#[test]
fn takes_a_5000_file_drop_hashing_each_file_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let hashed = Arc::new(AtomicUsize::new(0));
    let counter = hashed.clone();
    let mut setup = Setup::hashing(
        |folder| folder.modified = Modified::UploadAgain,
        std::sync::Arc::new(move |path| {
            counter.fetch_add(1, Ordering::SeqCst);
            crate::util::sha256_file(path).ok()
        }),
    );
    for index in 0..5000 {
        setup.write(&format!("{index:04}.txt"), b"some text");
    }
    let started = std::time::Instant::now();
    setup.engine.hint(&setup.folder().id, None);
    for _ in 0..40 {
        setup.tick();
        if setup.status().awaiting_confirmation == 5000 {
            break;
        }
    }
    assert_eq!(setup.status().awaiting_confirmation, 5000);
    setup.engine.confirm(&setup.folder().id, true);
    assert_eq!(setup.queued().len(), 5000);
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
    // Walked again while they're going up: nothing is hashed again.
    setup.engine.hint(&setup.folder().id, None);
    setup.run(5);
    assert_eq!(hashed.load(Ordering::SeqCst), 5000);
    // The pipeline gets the hash, so it doesn't read the file for it.
    assert!(setup.engine.host.queued.lock().unwrap().iter().all(|planned| planned.sha256.is_some()));
}

#[test]
fn the_windows_and_the_ticks_never_wait_for_a_hash() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let hashing = Arc::new(AtomicBool::new(false));
    let flag = hashing.clone();
    let mut setup = Setup::hashing(
        |folder| folder.modified = Modified::Overwrite,
        Arc::new(move |path| {
            flag.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1500));
            crate::util::sha256_file(path).ok()
        }),
    );
    let path = setup.write("big.bin", b"pretend this is big");
    setup.hint(&path);
    let mut now = setup.now;
    while !hashing.load(Ordering::SeqCst) {
        now = now.after(Duration::from_millis(500));
        let asked = std::time::Instant::now();
        setup.engine.tick(now);
        assert!(asked.elapsed() < Duration::from_millis(500), "a tick waited {:?}", asked.elapsed());
        std::thread::sleep(Duration::from_millis(5));
    }
    let asked = std::time::Instant::now();
    assert_eq!(setup.engine.overview(now).folders[0].waiting, 1);
    setup.engine.hint(&setup.folder().id, Some(setup.dir.join("other.bin")));
    setup.engine.tick(now.after(Duration::from_millis(500)));
    assert!(asked.elapsed() < Duration::from_millis(500), "{:?}", asked.elapsed());
    setup.now = now.after(Duration::from_millis(500));
    setup.run(3);
    assert_eq!(setup.queued(), vec!["big.bin".to_string()]);
}

#[test]
fn a_slow_hash_doesnt_hold_up_other_files() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let big_started = Arc::new(AtomicBool::new(false));
    let flag = big_started.clone();
    let mut setup = Setup::hashing(
        |folder| folder.modified = Modified::UploadAgain,
        Arc::new(move |path| {
            if path.ends_with("big.iso") {
                flag.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(3));
            }
            crate::util::sha256_file(path).ok()
        }),
    );
    let big = setup.write("big.iso", b"pretend this is 4 GB");
    setup.hint(&big);
    let started = std::time::Instant::now();
    let mut now = setup.now;
    while !big_started.load(Ordering::SeqCst) {
        now = now.after(Duration::from_millis(500));
        setup.engine.tick(now);
    }
    let small = setup.write("small.txt", b"small");
    setup.hint(&small);
    for _ in 0..40 {
        now = now.after(Duration::from_millis(500));
        setup.engine.tick(now);
        if setup.queued().contains(&"small.txt".to_string()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(setup.queued(), vec!["small.txt".to_string()]);
    assert!(started.elapsed() < Duration::from_millis(2500), "{:?}", started.elapsed());
    setup.now = now;
    setup.run(3);
    assert_eq!(setup.queued(), vec!["small.txt".to_string(), "big.iso".to_string()]);
}

#[test]
fn puts_off_the_safety_scan_while_the_energy_saver_is_on() {
    let mut setup = Setup::new(|_| {});
    setup.run(2);
    *setup.engine.host.saver.lock().unwrap() = true;
    // Only the safety scan would find it: no event.
    setup.write("quiet.png", b"png");
    setup.now = setup.now.after(Duration::from_secs(71 * 60));
    setup.run(5);
    assert!(setup.queued().is_empty());
    // Off again: the scan runs right away.
    *setup.engine.host.saver.lock().unwrap() = false;
    setup.engine.energy_saver_changed();
    setup.run(5);
    assert_eq!(setup.queued(), vec!["quiet.png".to_string()]);
}

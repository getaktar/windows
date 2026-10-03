// Typed wrappers around the Rust commands (src-tauri/src/commands.rs).
// Rust owns all state; windows read it through these and refresh when one of
// the `events` fires.

import { invoke } from "@tauri-apps/api/core";

export const events = {
  destinationsChanged: "destinations-changed",
  jobsChanged: "jobs-changed",
  historyChanged: "history-changed",
  settingsChanged: "settings-changed",
  languageChanged: "language-changed",
  localApiChanged: "local-api-changed",
  updateChanged: "update-changed",
  uploadSucceeded: "upload-succeeded",
  panelShown: "panel-shown",
  namesChanged: "names-changed",
  watchedChanged: "watched-changed",
  /** Settings should switch tabs or add a watched folder (`takeSettingsRequest`). */
  settingsRequest: "settings-request",
} as const;

export type ProviderPreset =
  | "cloudflareR2"
  | "amazonS3"
  | "minIO"
  | "backblazeB2"
  | "digitalOceanSpaces"
  | "customS3";

export interface DestinationConfig {
  id: string;
  name: string;
  preset: ProviderPreset;
  accountID?: string | null;
  endpoint: string;
  region: string;
  bucket: string;
  publicBaseURL: string;
  objectPathTemplate: string;
  forcePathStyle: boolean;
  isDefault: boolean;
  /** What's copied after an upload here; null follows Settings > Output.
   * With the fields below, destinations work as upload profiles
   * ("Builds", "Logs", "Screenshots"), even several on one bucket. */
  outputMode?: OutputMode | null;
  /** "Delete after" for uploads here, in days (0 keeps them); null until
   * it's picked for this destination, when Settings' last choice applies. */
  expiryDays?: number | null;
  /** Copy a temporary link valid this many seconds (one of
   * `temporaryLinkDurations`) instead of the public URL; null copies the
   * public URL. */
  temporaryLink?: number | null;
  /** What to strip from photos before they're uploaded; null removes the
   * location. */
  imageMetadata?: ImageMetadataPolicy | null;
  /** How folders are uploaded; null uploads them as a ZIP. */
  folderUpload?: FolderUploadMode | null;
  /** Converting, recompressing and resizing photos before they're
   * uploaded; null leaves them as they are. */
  imageProcessing?: ImageProcessing | null;
}

export type ImageFormat = "original" | "webp" | "avif";
export const imageFormats: ImageFormat[] = ["original", "webp", "avif"];

export interface ImageProcessing {
  format: ImageFormat;
  /** Lossy quality (one of `imageQualities`); null doesn't recompress. */
  quality: number | null;
  /** The longest side photos are scaled down to (one of `imageSizes`);
   * null keeps their size. */
  maxLongEdge: number | null;
}

/** "Light", "Medium" and "Strong" compression. */
export const imageQualities = [90, 80, 65] as const;
export const imageSizes = [3840, 2560, 1920, 1280, 1024] as const;

export type ImageMetadataPolicy = "removeLocation" | "removeAll" | "keepAll";
export const imageMetadataPolicies: ImageMetadataPolicy[] = ["removeLocation", "removeAll", "keepAll"];

export type FolderUploadMode = "zip" | "keepStructure";
export const folderUploadModes: FolderUploadMode[] = ["zip", "keepStructure"];

/** How long a temporary (presigned) link can stay valid, in seconds. Seven
 * days is the longest S3 allows. The minute-long ones are for confidential
 * files: S3 can't count downloads, so a link can't be single-use, but one
 * that dies minutes after it's sent is close. */
export const temporaryLinkDurations = [300, 900, 3600, 86_400, 604_800] as const;

export interface StorageCredentials {
  accessKeyId: string;
  secretAccessKey: string;
  sessionToken?: string | null;
}

export interface ConnectionResult {
  bucketReachable: boolean;
  writable: boolean;
  /** What opening the test file's public link returned; null when nothing
   * was uploaded to open. */
  publicLink: PublicLinkCheck | null;
}

export type PublicLinkCheck = { kind: "reachable" } | { kind: "status"; code: number } | { kind: "noResponse" };

export type JobState =
  | { kind: "waiting" }
  /** `resuming` while it continues an upload left unfinished before. */
  | { kind: "uploading"; progress: number; resuming: boolean }
  /** `reused` when an earlier upload of the same file was found, and its
   * link copied instead. */
  | { kind: "succeeded"; publicUrl: string; recordId: string; reused: boolean }
  | { kind: "failed"; message: string }
  | { kind: "cancelled" };

export interface Job {
  id: string;
  filename: string;
  destinationId: string;
  destinationName: string;
  state: JobState;
  /** The watched folder it came from, by name. */
  source?: string | null;
}

export interface UploadRecord {
  id: string;
  localFilename: string;
  objectKey: string;
  publicUrl: string;
  destinationId: string;
  destinationName: string;
  mimeType: string;
  byteSize: number;
  createdAt: number;
  /** When an expiring upload gets deleted (Unix milliseconds), or null. */
  expiresAt: number | null;
  /** SHA-256 of the bytes uploaded, when it was worked out. */
  contentHash?: string | null;
  /** "watchedFolder:<ID>" for a watched folder's uploads. */
  source?: string | null;
  /** The watched folder's name at the time. */
  sourceName?: string | null;
  hasThumbnail: boolean;
}

// MARK: - Watched folders (src-tauri/src/watched/model.rs)

export type Subfolders = "ignore" | "keepStructure" | "flatten";
export type FilterKind = "all" | "images" | "videos" | "screenshots" | "custom";
export type ModifiedPolicy = "ignore" | "uploadAgain" | "overwrite";
/** "tag" is the Mac's Finder tag; never offered on Windows. */
export type AfterUpload = "keep" | "trash" | "moveToUploaded" | "tag";
export type ClipboardPolicy = "copyLink" | "none";
/** What happens to an upload when its file is deleted from the folder. */
export type OnDeletePolicy = "keep" | "deleteRemote";
export type NotificationPolicy = "each" | "grouped" | "failuresOnly";

export interface WatchFilter {
  kind: FilterKind;
  include: string[];
  exclude: string[];
  minBytes: number | null;
  maxBytes: number | null;
}

export interface WatchHook {
  id: string;
  kind: "webhook" | "script";
  /** The webhook's URL, or the script's path. */
  target: string;
  enabled: boolean;
}

export interface WatchedFolder {
  id: string;
  name: string;
  path: string;
  enabled: boolean;
  preset: "screenshots" | null;
  /** null uploads to the default destination. */
  destinationID: string | null;
  /** null uses the destination's path template. */
  pathTemplate: string | null;
  subfolders: Subfolders;
  filter: WatchFilter;
  includeCloudOnly: boolean;
  modified: ModifiedPolicy;
  afterUpload: AfterUpload;
  /** Only works while the original stays in the folder (afterUpload "keep"). */
  onDelete: OnDeletePolicy;
  /** "Ask before deleting": every delete from the bucket waits for the user. */
  confirmDelete: boolean;
  clipboard: ClipboardPolicy;
  notifications: NotificationPolicy;
  /** null follows the destination; "public" or a temporary link's seconds. */
  temporaryLink: "public" | number | null;
  /** null follows the destination; 0 keeps uploads. */
  expiryDays: number | null;
  hooks: WatchHook[];
  addedAt: string;
}

export type WatchStatus = "watching" | "paused" | "disabled" | "accessNeeded" | "notFound" | "error";

export interface WatchedFolderInfo extends WatchedFolder {
  status: WatchStatus;
  waiting: number;
  uploading: number;
  failed: number;
  awaitingConfirmation: number;
  /** Uploads of deleted files on their way out of the bucket. */
  deleting: number;
  /** Deleted files whose uploads wait for "Delete from Bucket". */
  awaitingDeleteConfirmation: number;
  /** The file's name when exactly one waits. */
  awaitingDeleteName?: string | null;
  /** Unix milliseconds. */
  lastUploadAt: number | null;
  lastError: string | null;
}

export interface WatchOverview {
  paused: boolean;
  pauseReason: "user" | "battery" | "metered" | null;
  /** null, an ISO date, or "forever". */
  pausedUntil: string | null;
  pauseOnBattery: boolean;
  pauseOnMetered: boolean;
  folders: WatchedFolderInfo[];
}

export interface FolderCheck {
  path: string;
  name: string;
  /** Files already there that it would upload. */
  existingFiles: number;
}

export interface SettingsRequest {
  tab: string | null;
  watchPath: string | null;
  /** An aktar://import link to open "Import from Another Device" with. */
  importLink?: string | null;
}

export interface BucketObject {
  key: string;
  size: number;
  lastModified: number | null;
}

export interface BucketListing {
  prefix: string;
  folders: string[];
  objects: BucketObject[];
  nextContinuationToken: string | null;
}

export type OutputMode = "url" | "markdown" | "html" | "custom";

export interface Settings {
  outputMode: OutputMode;
  customTemplate: string;
  showNotification: boolean;
  closePanelAfterUpload: boolean;
  language: string | null;
  shortcut: string | null;
  /** "Rename and upload clipboard"; null when it isn't set. */
  renameShortcut: string | null;
  /** Copy the link of an earlier upload of the same file to the same
   * destination instead of uploading it again. */
  reuseDuplicateLinks: boolean;
  localApiEnabled: boolean;
  localApiPort: number;
  autoCheckUpdates: boolean;
  autoInstallUpdates: boolean;
  /** "Delete after": one of `expiryDurations`, or 0 to keep uploads. It
   * only applies to a destination whose lifecycle rules are active. */
  deleteAfterDays: number;
  /** The last lifecycle rules check of each destination, by ID. Left out
   * when there are none. */
  expiryRules?: Record<string, ExpiryRulesCheck>;
}

export type SettingsPatch = Partial<
  Pick<
    Settings,
    | "outputMode"
    | "customTemplate"
    | "showNotification"
    | "closePanelAfterUpload"
    | "reuseDuplicateLinks"
    | "autoCheckUpdates"
    | "autoInstallUpdates"
    | "deleteAfterDays"
  >
>;

/** The only "Delete after" choices, in days (src-tauri/src/expiry.rs). */
export const expiryDurations = [1, 7, 14, 30] as const;

export type ExpiryRulesStatus =
  | { kind: "active" }
  | { kind: "denied"; message: string }
  | { kind: "unsupported"; message: string };

export interface ExpiryRulesCheck {
  status: ExpiryRulesStatus;
  checkedAt: number;
}

/** What the destination form found out about the lifecycle rules while it
 * was open, recorded when the destination is saved. `check` is null when
 * auto-delete was turned off there. `connection` is the bucket that was
 * checked, so a result for a bucket the form no longer points at is ignored. */
export interface RulesConnection {
  endpoint: string;
  bucket: string;
  region: string;
}
export type FormRules =
  | { kind: "notChecked" }
  | { kind: "checked"; check: ExpiryRulesCheck | null; connection?: RulesConnection };

/** Whether the destination's bucket is known to have Aktar's lifecycle
 * rules, which is what makes "Delete after" available for it. */
export function expiryRulesActive(settings: Settings | null, destinationId: string | undefined) {
  if (!settings || !destinationId) return false;
  return settings.expiryRules?.[destinationId]?.status.kind === "active";
}

/** The "Delete after" picked for a destination, whether or not its bucket
 * has the rules yet. */
export function deleteAfterDays(settings: Settings | null, destination: DestinationConfig | undefined) {
  return destination?.expiryDays ?? settings?.deleteAfterDays ?? 0;
}

/** The "Delete after" that actually applies to a destination: its choice
 * once its rules are active, and 0 (Off) until then. */
export function effectiveDeleteAfterDays(settings: Settings | null, destination: DestinationConfig | undefined) {
  return expiryRulesActive(settings, destination?.id) ? deleteAfterDays(settings, destination) : 0;
}

export type LocalApiStatus =
  | { kind: "off" }
  | { kind: "starting" }
  | { kind: "running" }
  | { kind: "failed"; message: string };

export interface LocalApiState {
  enabled: boolean;
  port: number;
  token: string;
  status: LocalApiStatus;
}

export type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; version: string; notes: string | null }
  | { kind: "downloading"; version: string; progress: number | null }
  | { kind: "readyToInstall"; version: string }
  | { kind: "failed"; message: string };

export interface AppInfo {
  version: string;
  language: string;
  /** Windows' regional format (Settings > Time & language > Region), for
   * dates and numbers, which can differ from the display language. */
  regionLocale: string | null;
  /** Installed from the Microsoft Store, which handles updates. */
  packaged: boolean;
  languageOverride: string | null;
  languages: [string, string][];
}

export interface UploadSucceeded {
  destinationId: string;
  objectKey: string;
  byteSize: number;
}

/** A file waiting for its name in "Name This Upload". */
export interface NameRequest {
  id: string;
  /** The name without its extension, to start from. */
  name: string;
  /** The extension, which stays; empty when there's none. */
  extension: string;
}

/** A QR code's modules, row by row ("1" dark, "0" light), without the
 * quiet zone. */
export interface QrMatrix {
  size: number;
  modules: string;
}

/** "Share to Another Device": the link the QR code holds, and the code
 * ("XXXX-XXXX-XXXX") that's typed on the other device. */
export interface TransferShare {
  link: string;
  code: string;
}

/** What a transfer link carries. */
export interface TransferPayload {
  destination: DestinationConfig;
  credentials: StorageCredentials;
  /** The app-level template, sent along with a destination that copies
   * with it. */
  customTemplate: string | null;
}

/** Why a transfer link couldn't be opened; `checkTransferLink` and
 * `openTransfer` reject with one of these. */
export type TransferError = "notTransfer" | "newerVersion" | "wrongCode";

export type AppWindowName = "library" | "settings" | "onboarding" | "update";

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),

  listDestinations: () => invoke<DestinationConfig[]>("list_destinations"),
  saveDestination: (config: DestinationConfig, credentials: StorageCredentials | null, rules: FormRules) =>
    invoke<DestinationConfig>("save_destination", { config, credentials, rules }),
  removeDestination: (id: string) => invoke<void>("remove_destination", { id }),
  setDefaultDestination: (id: string) => invoke<void>("set_default_destination", { id }),
  /** A copy with the same keys, as a starting point for another profile on
   * the same bucket. */
  duplicateDestination: (id: string) => invoke<DestinationConfig>("duplicate_destination", { id }),
  /** A fresh code and link with the destination's keys; rejects with the
   * usual message when its keys are missing. */
  createTransfer: (destinationId: string) => invoke<TransferShare>("create_transfer", { destinationId }),
  checkTransferLink: (link: string) => invoke<void>("check_transfer_link", { link }),
  openTransfer: (link: string, code: string) => invoke<TransferPayload>("open_transfer", { link, code }),
  /** Saves an imported destination under its own ID, adding it or updating
   * the one here with that ID. */
  importDestination: (
    config: DestinationConfig,
    credentials: StorageCredentials | null,
    rules: FormRules,
    customTemplate: string | null,
  ) => invoke<DestinationConfig>("import_destination", { config, credentials, rules, customTemplate }),
  setDestinationExpiry: (id: string, days: number) => invoke<void>("set_destination_expiry", { id, days }),
  setDestinationLink: (id: string, seconds: number | null) => invoke<void>("set_destination_link", { id, seconds }),
  testConnection: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<ConnectionResult>("test_connection", { config, credentials }),
  expiryRulesStatus: (destinationId: string) => invoke<ExpiryRulesCheck | null>("expiry_rules_status", { destinationId }),
  setUpExpiryRules: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<ExpiryRulesCheck>("set_up_expiry_rules", { config, credentials }),
  /** The tmp/{N}d/ folders that already hold files and would start expiring
   * once the missing rules are set up. */
  expiryPrefixesInUse: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<string[]>("expiry_prefixes_in_use", { config, credentials }),
  removeExpiryRules: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<void>("remove_expiry_rules", { config, credentials }),

  /** Returns how many files and folders were queued: nothing is queued
   * when there's no destination (Rust then says so itself). With `rename`,
   * each file first waits for its name (`pendingNames`). */
  uploadFiles: (paths: string[], destinationId?: string, rename = false) =>
    invoke<number>("upload_files", { paths, destinationId: destinationId ?? null, rename }),
  uploadClipboard: () => invoke<boolean>("upload_clipboard"),
  pendingNames: () => invoke<NameRequest[]>("pending_names"),
  /** Uploads a file waiting for its name, or drops it (Cancel) for null. */
  resolveName: (id: string, name: string | null) => invoke<void>("resolve_name", { id, name }),
  /** Whether Alt is held right now (drops don't say). */
  altKeyDown: () => invoke<boolean>("alt_key_down"),
  listJobs: () => invoke<Job[]>("list_jobs"),
  retryJob: (id: string) => invoke<void>("retry_job", { id }),
  cancelJob: (id: string) => invoke<void>("cancel_job", { id }),
  dismissJob: (id: string) => invoke<void>("dismiss_job", { id }),

  listHistory: () => invoke<UploadRecord[]>("list_history"),
  thumbnailsDir: () => invoke<string>("thumbnails_dir"),
  /** A fresh presigned link to an upload in history. */
  recordTemporaryLink: (id: string, seconds: number) => invoke<string>("record_temporary_link", { id, seconds }),
  deleteRemote: (ids: string[]) => invoke<Record<string, string>>("delete_remote", { ids }),
  removeFromHistory: (ids: string[]) => invoke<void>("remove_from_history", { ids }),

  listObjects: (destinationId: string, prefix: string, continuationToken: string | null, recursive: boolean) =>
    invoke<BucketListing>("list_objects", { destinationId, prefix, continuationToken, recursive }),
  bucketDelete: (destinationId: string, key: string) => invoke<void>("bucket_delete", { destinationId, key }),
  bucketMove: (destinationId: string, from: string, to: string) =>
    invoke<string>("bucket_move", { destinationId, from, to }),
  bucketCreateFolder: (destinationId: string, prefix: string, name: string) =>
    invoke<string>("bucket_create_folder", { destinationId, prefix, name }),
  bucketPresign: (destinationId: string, key: string, seconds: number) =>
    invoke<string>("bucket_presign", { destinationId, key, seconds }),
  bucketUpload: (destinationId: string, paths: string[], prefix: string) =>
    invoke<number>("bucket_upload", { destinationId, paths, prefix }),
  fetchRemote: (url: string) => invoke<ArrayBuffer>("fetch_remote", { url }),

  qrCode: (text: string) => invoke<QrMatrix>("qr_code", { text }),
  copyQrImage: (text: string) => invoke<void>("copy_qr_image", { text }),
  saveQrImage: (text: string, path: string) => invoke<void>("save_qr_image", { text, path }),

  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: SettingsPatch) => invoke<Settings>("update_settings", { patch }),
  setShortcut: (accelerator: string | null) => invoke<void>("set_shortcut", { accelerator }),
  setRenameShortcut: (accelerator: string | null) => invoke<void>("set_rename_shortcut", { accelerator }),
  /** While recording a new shortcut, so pressing the current one records it
   * instead of uploading the clipboard. */
  setShortcutPaused: (paused: boolean) => invoke<void>("set_shortcut_paused", { paused }),
  setLanguage: (code: string | null) => invoke<string>("set_language", { code }),
  getLaunchAtLogin: () => invoke<boolean>("get_launch_at_login"),
  setLaunchAtLogin: (enabled: boolean) => invoke<boolean>("set_launch_at_login", { enabled }),

  localApiState: () => invoke<LocalApiState>("local_api_state"),
  setLocalApiEnabled: (enabled: boolean) => invoke<void>("set_local_api_enabled", { enabled }),
  setLocalApiPort: (port: number) => invoke<void>("set_local_api_port", { port }),
  regenerateApiToken: () => invoke<void>("regenerate_api_token"),

  copyText: (text: string) => invoke<void>("copy_text", { text }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  openWindow: (name: AppWindowName) => invoke<void>("open_window", { name }),
  closeWindow: (name: AppWindowName) => invoke<void>("close_window", { name }),
  showPanel: () => invoke<void>("show_panel"),
  /** Closes Welcome, then shows the panel once focus has settled. */
  finishOnboarding: () => invoke<void>("finish_onboarding"),
  hidePanel: () => invoke<void>("hide_panel"),
  setPanelShowingDialog: (showing: boolean) => invoke<void>("set_panel_showing_dialog", { showing }),
  setPanelHeight: (height: number) => invoke<void>("set_panel_height", { height }),
  quitApp: () => invoke<void>("quit_app"),

  watchedFolders: () => invoke<WatchOverview>("watched_folders"),
  /** Refused folders reject with the reason. */
  checkWatchFolder: (path: string) => invoke<FolderCheck>("check_watch_folder", { path }),
  addWatchedFolder: (path: string, uploadExisting: boolean) =>
    invoke<WatchedFolder>("add_watched_folder", { path, uploadExisting }),
  addScreenshotsFolder: () => invoke<WatchedFolder>("add_screenshots_folder"),
  saveWatchedFolder: (folder: WatchedFolder) => invoke<void>("save_watched_folder", { folder }),
  removeWatchedFolder: (id: string) => invoke<void>("remove_watched_folder", { id }),
  resetWatchedFolder: (id: string) => invoke<void>("reset_watched_folder", { id }),
  setWatchedFolderEnabled: (id: string, enabled: boolean) => invoke<void>("set_watched_folder_enabled", { id, enabled }),
  /** "Upload" (true) or "Skip" on a large batch. */
  confirmWatchedBatch: (id: string, upload: boolean) => invoke<void>("confirm_watched_batch", { id, upload }),
  /** "Delete from Bucket" (true) or "Keep Uploaded Files" on a large deletion. */
  confirmWatchedDeletions: (id: string, deleteRemote: boolean) => invoke<void>("confirm_watched_deletions", { id, delete: deleteRemote }),
  uploadWatchedPending: (id: string) => invoke<void>("upload_watched_pending", { id }),
  retryWatchedFailed: (id: string) => invoke<void>("retry_watched_failed", { id }),
  /** For `minutes`, until midnight with `untilTomorrow`, or until resumed. */
  pauseWatching: (minutes: number | null, untilTomorrow = false) => invoke<void>("pause_watching", { minutes, untilTomorrow }),
  resumeWatching: () => invoke<void>("resume_watching"),
  setWatchPauseConditions: (onBattery: boolean | null, onMetered: boolean | null) =>
    invoke<void>("set_watch_pause_conditions", { onBattery, onMetered }),
  testWatchHook: (folder: WatchedFolder, hook: WatchHook) => invoke<void>("test_watch_hook", { folder, hook }),
  showFolder: (path: string) => invoke<void>("show_folder", { path }),
  takeSettingsRequest: () => invoke<SettingsRequest>("take_settings_request"),

  updateStatus: () => invoke<UpdateStatus>("update_status"),
  checkForUpdates: () => invoke<void>("check_for_updates"),
  installUpdate: () => invoke<void>("install_update"),
};

/** Errors from commands arrive as plain strings. */
export function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

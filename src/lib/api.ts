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
}

export interface StorageCredentials {
  accessKeyId: string;
  secretAccessKey: string;
  sessionToken?: string | null;
}

export interface ConnectionResult {
  bucketReachable: boolean;
  writable: boolean;
  publicUrlReachable: boolean | null;
}

export type JobState =
  | { kind: "waiting" }
  | { kind: "uploading"; progress: number }
  | { kind: "succeeded"; publicUrl: string; recordId: string }
  | { kind: "failed"; message: string }
  | { kind: "cancelled" };

export interface Job {
  id: string;
  filename: string;
  destinationId: string;
  destinationName: string;
  state: JobState;
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
  hasThumbnail: boolean;
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
 * auto-delete was turned off there. */
export type FormRules = { kind: "notChecked" } | { kind: "checked"; check: ExpiryRulesCheck | null };

/** Whether the destination's bucket is known to have Aktar's lifecycle
 * rules, which is what makes "Delete after" available for it. */
export function expiryRulesActive(settings: Settings | null, destinationId: string | undefined) {
  if (!settings || !destinationId) return false;
  return settings.expiryRules?.[destinationId]?.status.kind === "active";
}

/** The "Delete after" that actually applies to a destination: the setting
 * once its rules are active, and 0 (Off) until then. */
export function effectiveDeleteAfterDays(settings: Settings | null, destinationId: string | undefined) {
  return expiryRulesActive(settings, destinationId) ? (settings?.deleteAfterDays ?? 0) : 0;
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

export type AppWindowName = "library" | "settings" | "onboarding" | "update";

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),

  listDestinations: () => invoke<DestinationConfig[]>("list_destinations"),
  saveDestination: (config: DestinationConfig, credentials: StorageCredentials | null, rules: FormRules) =>
    invoke<DestinationConfig>("save_destination", { config, credentials, rules }),
  removeDestination: (id: string) => invoke<void>("remove_destination", { id }),
  setDefaultDestination: (id: string) => invoke<void>("set_default_destination", { id }),
  testConnection: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<ConnectionResult>("test_connection", { config, credentials }),
  expiryRulesStatus: (destinationId: string) => invoke<ExpiryRulesCheck | null>("expiry_rules_status", { destinationId }),
  setUpExpiryRules: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<ExpiryRulesCheck>("set_up_expiry_rules", { config, credentials }),
  removeExpiryRules: (config: DestinationConfig, credentials: StorageCredentials | null) =>
    invoke<void>("remove_expiry_rules", { config, credentials }),

  /** Returns how many files were queued: folders are skipped, and nothing
   * is queued when there's no destination (Rust then says so itself). */
  uploadFiles: (paths: string[], destinationId?: string) =>
    invoke<number>("upload_files", { paths, destinationId: destinationId ?? null }),
  uploadClipboard: () => invoke<boolean>("upload_clipboard"),
  listJobs: () => invoke<Job[]>("list_jobs"),
  retryJob: (id: string) => invoke<void>("retry_job", { id }),
  cancelJob: (id: string) => invoke<void>("cancel_job", { id }),
  dismissJob: (id: string) => invoke<void>("dismiss_job", { id }),

  listHistory: () => invoke<UploadRecord[]>("list_history"),
  thumbnailsDir: () => invoke<string>("thumbnails_dir"),
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

  getSettings: () => invoke<Settings>("get_settings"),
  updateSettings: (patch: SettingsPatch) => invoke<Settings>("update_settings", { patch }),
  setShortcut: (accelerator: string | null) => invoke<void>("set_shortcut", { accelerator }),
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

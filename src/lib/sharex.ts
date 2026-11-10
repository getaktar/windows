// "Import from ShareX" (src-tauri/src/sharex): ShareX's S3 and Backblaze B2
// uploaders as destinations, its shortener custom uploaders as short link
// settings, and its history. The secret keys stay in Rust between the
// review and the import.

import { invoke } from "@tauri-apps/api/core";

import type { DestinationConfig, ProviderPreset, ShareXSecretLocation } from "./api";

export interface ShareXStatus {
  /** ShareX's personal folder, when it's on this PC. */
  folder: string | null;
  /** Its settings are there to import. */
  hasSettings: boolean;
  /** The panel offers the import once. */
  showBanner: boolean;
}

export interface ShareXReviewDestination {
  /** The review's own ID for it. */
  id: string;
  kind: "amazonS3" | "backblazeB2";
  name: string;
  preset: ProviderPreset;
  bucket: string;
  endpoint: string;
  publicBaseURL: string;
  template: string;
  notes: string[];
  needsSecret: boolean;
  needsEndpoint: boolean;
  alreadyInAktar: boolean;
}

export interface ShareXReviewShortener {
  id: string;
  name: string;
  host: string;
  method: string;
  endpoint: string;
  usesHTTP: boolean;
  deletionSkipped: boolean;
  hasToken: boolean;
  secretLocations: ShareXSecretLocation[];
}

export interface ShareXReview {
  /** The folder or file it was read from. */
  source: string;
  destinations: ShareXReviewDestination[];
  shorteners: ShareXReviewShortener[];
  historyCount: number;
  historySkipped: number;
  skipped: { name: string; reason: string }[];
}

export interface ShareXImportChoices {
  destinations: { id: string; secretAccessKey?: string | null; endpoint?: string | null }[];
  /** `destination`: a review destination's ID, or one already in Aktar. */
  shorteners: { id: string; destination: string }[];
  history: boolean;
}

export interface ShareXImported {
  destinations: DestinationConfig[];
  failures: { name: string; message: string }[];
  shorteners: number;
  historyImported: number;
  historyDuplicates: number;
}

export const sharex = {
  status: () => invoke<ShareXStatus>("sharex_status"),
  dismissOffer: () => invoke<void>("sharex_dismiss_offer"),
  openImport: () => invoke<void>("sharex_open_import"),
  /** What's in ShareX's folder; null when its settings aren't there. */
  scan: () => invoke<ShareXReview | null>("sharex_scan"),
  /** "Choose File…"; null when the dialog was cancelled. */
  pickFile: () => invoke<ShareXReview | null>("sharex_pick_file"),
  importReviewed: (choices: ShareXImportChoices) => invoke<ShareXImported>("sharex_import", { choices }),
};

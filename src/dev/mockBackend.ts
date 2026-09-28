// A stand-in for the Rust side, so the UI can be worked on in a regular
// browser on any OS: `pnpm dev:mock`, then open
// http://localhost:1420/#/library (or #/panel, #/settings, #/onboarding,
// #/update). Add ?lang=tr (etc.) before the # to preview a translation.
// Only loaded in mock mode; production builds drop it entirely.

import { mockConvertFileSrc, mockIPC, mockWindows } from "@tauri-apps/api/mocks";

import type {
  BucketListing,
  DestinationConfig,
  Job,
  LocalApiState,
  Settings,
  UpdateStatus,
  UploadRecord,
} from "../lib/api";

// VITE_MOCK_SCENE=store: tidy sample data for Microsoft Store screenshots
// (one upload in progress, no errors, no update banner). VITE_MOCK_LANG sets
// the language where there's no ?lang= to read, e.g. in the real app's
// windows pointed at the mock dev server.
const storeScene = import.meta.env.VITE_MOCK_SCENE === "store";

const hour = 3_600_000;
const now = Date.now();

const destinations: DestinationConfig[] = [
  {
    id: "D1",
    name: "Screenshots",
    preset: "cloudflareR2",
    accountID: "0123456789abcdef",
    endpoint: "https://0123456789abcdef.r2.cloudflarestorage.com",
    region: "auto",
    bucket: "screenshots",
    publicBaseURL: "img.example.com",
    objectPathTemplate: "{year}/{month}/{uuid}.{ext}",
    forcePathStyle: false,
    isDefault: true,
  },
  {
    id: "D2",
    name: "Backups",
    preset: "backblazeB2",
    endpoint: "s3.us-west-004.backblazeb2.com",
    region: "us-west-004",
    bucket: "team-backups",
    publicBaseURL: "files.example.com",
    objectPathTemplate: "{date}/{filename}.{ext}",
    forcePathStyle: false,
    isDefault: false,
  },
];

const photo = (id: number) => `https://picsum.photos/id/${id}/1200/800.jpg`;

let history: UploadRecord[] = [
  ["Screenshot 2026-09-28 at 10.14.png", photo(1015), "D1", "image/png", 482_113, now - 0.2 * hour],
  ["release-notes.md", "https://raw.githubusercontent.com/tauri-apps/tauri/dev/README.md", "D1", "text/markdown", 5_210, now - 2 * hour],
  ["mountains.jpg", photo(1018), "D1", "image/jpeg", 1_804_551, now - 26 * hour],
  ["invoice-0921.pdf", "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf", "D2", "application/pdf", 13_264, now - 30 * hour],
  ["project-archive.zip", "https://files.example.com/2026-09-20/project-archive.zip", "D2", "application/zip", 48_220_113, now - 8 * 24 * hour],
  ["lake.jpg", photo(1036), "D1", "image/jpeg", 922_004, now - 40 * 24 * hour],
].map(([name, url, destinationId, mimeType, size, createdAt], index) => ({
  id: `R${index}`,
  localFilename: name as string,
  objectKey: `2026/09/${name}`,
  publicUrl: url as string,
  destinationId: destinationId as string,
  destinationName: destinations.find((d) => d.id === destinationId)!.name,
  mimeType: mimeType as string,
  byteSize: size as number,
  createdAt: createdAt as number,
  hasThumbnail: false,
}));

let jobs: Job[] = storeScene
  ? [{ id: "J1", filename: "screen-recording.mp4", destinationId: "D1", destinationName: "Screenshots", state: { kind: "uploading", progress: 0.62 } }]
  : [
      { id: "J1", filename: "screen-recording.mp4", destinationId: "D1", destinationName: "Screenshots", state: { kind: "uploading", progress: 0 } },
      {
        id: "J2",
        filename: "huge-export.csv",
        destinationId: "D1",
        destinationName: "Screenshots",
        state: { kind: "failed", message: "Could not authenticate. Check your Access Key ID and Secret Access Key." },
      },
    ];

let settings: Settings = {
  outputMode: "url",
  customTemplate: "![{filename}]({url})",
  showNotification: true,
  closePanelAfterUpload: true,
  language: null,
  shortcut: "Ctrl+Shift+Alt+KeyU",
  localApiEnabled: true,
  localApiPort: 47913,
  autoCheckUpdates: true,
  autoInstallUpdates: false,
};

const localApi: LocalApiState = {
  enabled: true,
  port: 47913,
  token: "example-token-not-real",
  status: { kind: "running" },
};

const update: UpdateStatus = storeScene
  ? { kind: "idle" }
  : { kind: "available", version: "0.2.0", notes: "Faster uploads and a new bucket search." };

function listing(prefix: string, recursive: boolean): BucketListing {
  const objects = [
    { key: "2026/09/cover.png", size: 381_220, lastModified: now - 3 * hour },
    { key: "2026/09/notes.md", size: 2_048, lastModified: now - 5 * hour },
    { key: "2026/09/report.pdf", size: 734_003, lastModified: now - 50 * hour },
    { key: "2026/08/archive.zip", size: 12_004_331, lastModified: now - 30 * 24 * hour },
    { key: "readme.txt", size: 912, lastModified: now - 90 * 24 * hour },
  ];
  if (recursive) {
    return { prefix, folders: ["2026/", "2026/09/", "2026/08/"], objects: objects.filter((o) => o.key.startsWith(prefix)), nextContinuationToken: null };
  }
  const direct = objects.filter((o) => o.key.startsWith(prefix) && !o.key.slice(prefix.length).includes("/"));
  const folders = [
    ...new Set(
      objects
        .filter((o) => o.key.startsWith(prefix) && o.key.slice(prefix.length).includes("/"))
        .map((o) => prefix + o.key.slice(prefix.length).split("/")[0] + "/"),
    ),
  ];
  return { prefix, folders, objects: direct, nextContinuationToken: null };
}

export function installMockBackend(route: string) {
  const language =
    new URLSearchParams(window.location.search).get("lang") ?? import.meta.env.VITE_MOCK_LANG ?? "en";
  mockWindows(route || "panel");
  mockConvertFileSrc("windows");
  mockIPC(
    (cmd, payload) => {
      const args = (payload ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case "app_info":
          return {
            version: "0.1.0",
            language,
            regionLocale: null,
            packaged: storeScene,
            languageOverride: null,
            languages: [
              ["en", "English"], ["tr", "Türkçe"], ["de", "Deutsch"], ["fr", "Français"],
              ["es", "Español"], ["pt-BR", "Português (Brasil)"], ["ja", "日本語"], ["zh-Hans", "简体中文"],
            ],
          };
        case "list_destinations":
          return destinations;
        case "list_jobs":
          return jobs;
        case "list_history":
          return history;
        case "thumbnails_dir":
          return "C:\\Users\\you\\AppData\\Local\\com.getaktar.windows\\thumbnails";
        case "remove_from_history":
          history = history.filter((record) => !(args.ids as string[]).includes(record.id));
          return null;
        case "delete_remote":
          return {};
        case "get_settings":
          return settings;
        case "update_settings":
          settings = { ...settings, ...(args.patch as Partial<Settings>) };
          return settings;
        case "get_launch_at_login":
          return true;
        case "local_api_state":
          return localApi;
        case "update_status":
          return update;
        case "list_objects":
          return listing(args.prefix as string, args.recursive as boolean);
        case "bucket_presign":
          return photo(1025);
        case "fetch_remote":
          return fetch(args.url as string).then((response) => response.arrayBuffer());
        case "test_connection":
          return { bucketReachable: true, writable: true, publicUrlReachable: true };
        case "upload_clipboard":
          return false;
        case "upload_files":
        case "bucket_upload":
          return (args.paths as string[]).length;
        case "dismiss_job":
        case "cancel_job":
          jobs = jobs.filter((job) => job.id !== args.id);
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
}

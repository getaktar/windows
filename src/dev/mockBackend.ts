// A stand-in for the Rust side, so the UI can be worked on in a regular
// browser on any OS: `pnpm dev:mock`, then open
// http://localhost:1420/#/library (or #/panel, #/settings, #/onboarding,
// #/update). Add ?lang=tr (etc.) before the # to preview a translation.
// Only loaded in mock mode; production builds drop it entirely.

import { emit } from "@tauri-apps/api/event";
import { mockConvertFileSrc, mockIPC, mockWindows } from "@tauri-apps/api/mocks";

import type {
  BucketListing,
  DestinationConfig,
  Job,
  LocalApiState,
  NameRequest,
  QrMatrix,
  Settings,
  UpdateStatus,
  UploadRecord,
  WatchedFolder,
  WatchOverview,
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
    imageProcessing: { format: "webp", quality: 80, maxLongEdge: 2560 },
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
  // Two expiring uploads (one about to go) outside the Store scene.
  expiresAt: storeScene ? null : index === 0 ? (createdAt as number) + 7 * 24 * hour : index === 2 ? now + 0.5 * hour : null,
  hasThumbnail: false,
  ...(index === 0 ? { source: "watchedFolder:W1", sourceName: "Screenshots" } : {}),
}));

let jobs: Job[] = storeScene
  ? [{ id: "J1", filename: "screen-recording.mp4", destinationId: "D1", destinationName: "Screenshots", state: { kind: "uploading", progress: 0.62, resuming: false } }]
  : [
      { id: "J1", filename: "screen-recording.mp4", destinationId: "D1", destinationName: "Screenshots", state: { kind: "uploading", progress: 0, resuming: false } },
      {
        id: "J3",
        filename: "disk-image.iso",
        destinationId: "D1",
        destinationName: "Screenshots",
        state: { kind: "uploading", progress: 0.41, resuming: true },
      },
      {
        id: "J4",
        filename: "Screenshot 2026-10-02 091233.png",
        destinationId: "D1",
        destinationName: "Screenshots",
        state: { kind: "uploading", progress: 0.3, resuming: false },
        source: "Screenshots",
      },
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
  renameShortcut: null,
  reuseDuplicateLinks: true,
  localApiEnabled: true,
  localApiPort: 47913,
  autoCheckUpdates: true,
  autoInstallUpdates: false,
  deleteAfterDays: storeScene ? 0 : 7,
  // The default destination has the rules; the other one doesn't yet.
  expiryRules: storeScene ? undefined : { D1: { status: { kind: "active" }, checkedAt: Date.now() - 3 * hour } },
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

/** ?nowatch starts without watched folders, to see the empty state. */
const watchedBase = (id: string, name: string, path: string): WatchedFolder => ({
  id,
  name,
  path,
  enabled: true,
  preset: null,
  destinationID: null,
  pathTemplate: null,
  subfolders: "ignore",
  filter: { kind: "all", include: [], exclude: [], minBytes: null, maxBytes: null },
  includeCloudOnly: false,
  modified: "ignore",
  afterUpload: "keep",
  onDelete: "keep",
  confirmDelete: true,
  clipboard: "none",
  notifications: "grouped",
  temporaryLink: null,
  expiryDays: null,
  hooks: [],
  addedAt: new Date(now - 24 * hour).toISOString(),
});

let watched: WatchOverview = {
  paused: false,
  pauseReason: null,
  pausedUntil: null,
  pauseOnBattery: false,
  pauseOnMetered: true,
  folders: new URLSearchParams(window.location.search).has("nowatch")
    ? []
    : [
        {
          ...watchedBase("W1", "Screenshots", "C:\\Users\\you\\Pictures\\Screenshots"),
          preset: "screenshots",
          onDelete: "deleteRemote",
          filter: { kind: "screenshots", include: [], exclude: [], minBytes: null, maxBytes: null },
          clipboard: "copyLink",
          notifications: "each",
          status: "watching",
          waiting: 1,
          uploading: 1,
          failed: 0,
          awaitingConfirmation: 0,
          deleting: 0,
          awaitingDeleteConfirmation: storeScene ? 0 : 1,
          awaitingDeleteName: "Screenshot 2026-10-01 181512.png",
          lastUploadAt: now - 0.2 * hour,
          lastError: null,
        },
        {
          ...watchedBase("W2", "Exports", "D:\\Projects\\Client Work\\2026\\Quarterly Reports\\Exports"),
          destinationID: "D2",
          subfolders: "keepStructure",
          afterUpload: "moveToUploaded",
          hooks: [{ id: "H1", kind: "webhook", target: "https://hooks.example.com/aktar", enabled: true }],
          status: "watching",
          waiting: 0,
          uploading: 0,
          failed: storeScene ? 0 : 2,
          awaitingConfirmation: storeScene ? 0 : 312,
          deleting: 0,
          awaitingDeleteConfirmation: 0,
          lastUploadAt: now - 26 * hour,
          lastError: storeScene ? null : "Upload interrupted. The connection stopped responding.",
        },
      ],
};

function setWatched(change: Partial<WatchOverview>) {
  watched = { ...watched, ...change };
  window.setTimeout(() => emit("watched-changed"), 0);
  return null;
}

/** Files dropped with Alt held (?alt in the URL), waiting for a name;
 * ?rename starts with one, to see the dialog. */
let pendingNames: NameRequest[] = new URLSearchParams(window.location.search).has("rename")
  ? [{ id: "N1", name: "Screenshot 2026-10-01 at 09.12", extension: "png" }]
  : [];

/** A stand-in QR code: the three finder squares and a scatter of modules
 * from the text, enough to see the layout (it doesn't scan). */
function mockQr(text: string): QrMatrix {
  const size = 33;
  let seed = [...text].reduce((hash, char) => (hash * 31 + char.charCodeAt(0)) >>> 0, 7);
  const finder = (row: number, column: number) => {
    for (const [top, left] of [[0, 0], [0, size - 7], [size - 7, 0]]) {
      const [r, c] = [row - top, column - left];
      if (r >= 0 && r < 7 && c >= 0 && c < 7) return r === 0 || r === 6 || c === 0 || c === 6 || (r >= 2 && r <= 4 && c >= 2 && c <= 4) ? 1 : 0;
      if (r >= -1 && r <= 7 && c >= -1 && c <= 7) return 0;
    }
    return null;
  };
  let modules = "";
  for (let row = 0; row < size; row++) {
    for (let column = 0; column < size; column++) {
      seed = (seed * 1103515245 + 12345) >>> 0;
      modules += String(finder(row, column) ?? (seed >>> 16) % 2);
    }
  }
  return { size, modules };
}

/** "Share to Another Device" and "Import from Another Device": a link from
 * the shared test vectors, whose code is K7P2-QX9M-4TRW. ?dup makes the
 * imported destination one that's already here, uploading to the same
 * place; ?dup=moved makes it upload to another bucket, which "Update
 * Existing" warns about. ?testfail makes the connection test after the
 * import fail. */
const mockTransferLink =
  "aktar://import#AQABAgMEBQYHCAkKCwwNDg-goaKjpKWmp6ipqqtb3kuNh7HmTZAAsoxnF4uSJvWbwAcgdjc8HykCY2sexEkIGCNmWERsoA2f-1YQqaZAM-KR1X_z2vLMQxulRNBFXgNXHOZ76srLl3KfOH8DqV0aBtQyW5nvSf1IdulSa9cqDNfjMUPAycY-CKM_l2Kvs_FeyQXUAR9PGWEsgdNp4BwpIZVOUhr41EdisZOp9Jw5lwR1dHg4ADawbqib1DHbyu0n3uDcoHJrRkbBIZfGppFzOiRT7ZLEWUyI1OFgEzkNpnoNtESI2Z9nFS3jk1cMcEx0YUxqHJo1EwgAWVdoLeZi9gK76SsoT-CpvpZqR56eTh9pNp_dDlOg_4lYUSLVrikhpa6O3GJsGSoSdkg6g9f2Em00M2ADtYjB5y3stTrUTr4a1vbn__r09ean6d4l2d9olT2WQbjB0vS4TFKM_hO9Cuf3kb_GvGVk2tivHSupPTjVjFzcZaP2NaQOnwfbdxLPnkOq7XBsVpun04vHtvTX4h2nQ1a8KNyCI6tlJZO-a8vJkKJrdl0n-kkiCVrxD2SkP5zmzO38eOaCfgfJWgvsGGcyIntXUEy8A09FoOZ441goeDiIHrruFOXHOyNclHP2dLtVPlMGzKCIDra0JXr1";

function mockTransferPayload() {
  const duplicate = new URLSearchParams(window.location.search).get("dup");
  if (duplicate !== null) {
    const existing = destinations.find((destination) => destination.id === "D1")!;
    return {
      destination: {
        ...existing,
        // Written differently on purpose: the same place, so no warning.
        endpoint: `${existing.endpoint.replace(/^https:\/\//, "").toUpperCase()}/`,
        bucket: duplicate === "moved" ? "someone-elses-bucket" : ` ${existing.bucket}`,
        isDefault: false,
      },
      credentials: { accessKeyId: "EXAMPLEACCESSKEYID000", secretAccessKey: "example-secret-not-real-0000000000000000" },
      customTemplate: null,
    };
  }
  return {
    destination: {
      id: "0E984725-C51C-4BF4-9960-E1C80E27ABA0",
      name: "MinIO",
      preset: "minIO",
      endpoint: "http://192.168.1.10:9000",
      region: "us-east-1",
      bucket: "uploads",
      publicBaseURL: "http://192.168.1.10:9000/uploads",
      objectPathTemplate: "{uuid}.{ext}",
      forcePathStyle: true,
      isDefault: false,
    },
    credentials: { accessKeyId: "minioadmin", secretAccessKey: "minioadmin" },
    customTemplate: null,
  };
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
        // A new array, as from Rust, so a change shows up in React state.
        case "list_destinations":
          return [...destinations];
        case "list_jobs":
          return jobs;
        case "list_history":
          return history;
        case "load_record_thumbnail":
          return false;
        case "bucket_thumbnail":
          return null;
        case "thumbnail_usage":
          return 4_812_800;
        case "clear_thumbnails":
          return null;
        case "thumbnail_cleanup_prefix":
          return null;
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
        case "record_temporary_link":
          return photo(1025);
        case "set_destination_link": {
          const destination = destinations.find((d) => d.id === args.id);
          if (destination) destination.temporaryLink = (args.seconds as number | null) ?? null;
          return null;
        }
        case "set_destination_expiry": {
          const destination = destinations.find((d) => d.id === args.id);
          if (destination) destination.expiryDays = args.days as number;
          return null;
        }
        case "fetch_remote":
          return fetch(args.url as string).then((response) => response.arrayBuffer());
        case "expiry_rules_status":
          return settings.expiryRules?.[args.destinationId as string] ?? null;
        case "set_up_expiry_rules":
          return {
            status: { kind: "denied", message: "Access Denied" },
            checkedAt: Date.now(),
          };
        case "expiry_prefixes_in_use":
          return [];
        case "remove_expiry_rules":
          return null;
        // ?testfail: the bucket can't be reached at all.
        case "test_connection":
          return new Promise((resolve, reject) =>
            window.setTimeout(
              () =>
                new URLSearchParams(window.location.search).has("testfail")
                  ? reject("error sending request for url (http://192.168.1.10:9000/uploads)")
                  : resolve({ bucketReachable: true, writable: true, publicLink: { kind: "status", code: 403 } }),
              700,
            ),
          );
        case "upload_clipboard":
          return false;
        case "upload_files":
          if (args.rename || new URLSearchParams(window.location.search).has("alt")) {
            for (const path of args.paths as string[]) {
              const filename = path.split(/[\\/]/).pop() ?? path;
              const dot = filename.lastIndexOf(".");
              pendingNames.push({
                id: `N${Date.now()}${pendingNames.length}`,
                name: dot > 0 ? filename.slice(0, dot) : filename,
                extension: dot > 0 ? filename.slice(dot + 1) : "",
              });
            }
          }
          return (args.paths as string[]).length;
        case "bucket_upload":
          return (args.paths as string[]).length;
        case "pending_names":
          return pendingNames;
        case "resolve_name":
          pendingNames = pendingNames.filter((request) => request.id !== args.id);
          return null;
        case "alt_key_down":
          return new URLSearchParams(window.location.search).has("alt");
        case "qr_code":
          return mockQr(args.text as string);
        case "create_transfer":
          return { link: mockTransferLink, code: "K7P2-QX9M-4TRW" };
        // The share dialog keeps its window out of screenshots.
        case "plugin:window|set_content_protected":
          return null;
        case "check_transfer_link": {
          const link = String(args.link).trim();
          return /^aktar:\/\/import#[\w-]{60,}$/i.test(link) || /^[\w-]{60,}$/.test(link) ? null : Promise.reject("notTransfer");
        }
        case "open_transfer":
          return String(args.code).toUpperCase().replace(/[\s-]/g, "") === "K7P2QX9M4TRW"
            ? mockTransferPayload()
            : Promise.reject("wrongCode");
        // Rust trims what it decodes; the mock payload keeps a space to
        // show that it doesn't count as another bucket.
        case "import_destination": {
          const imported = args.config as DestinationConfig;
          const index = destinations.findIndex((destination) => destination.id === imported.id);
          const config = {
            ...imported,
            bucket: imported.bucket.trim(),
            isDefault: index >= 0 ? destinations[index].isDefault : destinations.length === 0,
          };
          if (index >= 0) destinations[index] = config;
          else destinations.push(config);
          window.setTimeout(() => emit("destinations-changed"), 0);
          return config;
        }
        case "set_rename_shortcut":
          settings = { ...settings, renameShortcut: (args.accelerator as string | null) ?? null };
          return null;
        case "set_shortcut":
          settings = { ...settings, shortcut: (args.accelerator as string | null) ?? null };
          return null;
        case "watched_folders":
          return watched;
        case "check_watch_folder":
          return { path: args.path, name: String(args.path).split(/[\\/]/).pop(), existingFiles: 37 };
        case "add_watched_folder":
        case "add_screenshots_folder": {
          const path = (args.path as string | undefined) ?? "C:\\Users\\you\\Pictures\\Screenshots";
          const folder = watchedBase(`W${Date.now()}`, path.split(/[\\/]/).pop() ?? path, path);
          setWatched({
            folders: [
              ...watched.folders,
              {
                ...folder,
                status: "watching",
                waiting: 0,
                uploading: 0,
                failed: 0,
                awaitingConfirmation: 0,
                deleting: 0,
                awaitingDeleteConfirmation: 0,
                lastUploadAt: null,
                lastError: null,
              },
            ],
          });
          return folder;
        }
        case "save_watched_folder": {
          const saved = args.folder as WatchedFolder;
          return setWatched({ folders: watched.folders.map((folder) => (folder.id === saved.id ? { ...folder, ...saved } : folder)) });
        }
        case "remove_watched_folder":
          return setWatched({ folders: watched.folders.filter((folder) => folder.id !== args.id) });
        case "set_watched_folder_enabled":
          return setWatched({
            folders: watched.folders.map((folder) =>
              folder.id === args.id ? { ...folder, enabled: args.enabled as boolean, status: args.enabled ? "watching" : "disabled" } : folder,
            ),
          });
        case "confirm_watched_batch":
          return setWatched({ folders: watched.folders.map((folder) => (folder.id === args.id ? { ...folder, awaitingConfirmation: 0 } : folder)) });
        case "confirm_watched_deletions":
          return setWatched({ folders: watched.folders.map((folder) => (folder.id === args.id ? { ...folder, awaitingDeleteConfirmation: 0 } : folder)) });
        case "pause_watching": {
          const until = args.untilTomorrow
            ? new Date(new Date().setHours(24, 0, 0, 0)).toISOString()
            : args.minutes
              ? new Date(Date.now() + (args.minutes as number) * 60_000).toISOString()
              : "forever";
          return setWatched({ paused: true, pauseReason: "user", pausedUntil: until });
        }
        case "resume_watching":
          return setWatched({ paused: false, pauseReason: null, pausedUntil: null });
        case "set_watch_pause_conditions":
          return setWatched({
            pauseOnBattery: (args.onBattery as boolean | null) ?? watched.pauseOnBattery,
            pauseOnMetered: (args.onMetered as boolean | null) ?? watched.pauseOnMetered,
          });
        case "take_settings_request":
          return { tab: new URLSearchParams(window.location.search).get("tab"), watchPath: null };
        case "test_watch_hook":
          return null;
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

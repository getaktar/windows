import type { OutputMode, ProviderPreset } from "./api";
import type { Translate } from "./i18n";

/** Splits like NSString.pathExtension: ".env" has no extension. */
export function splitExtension(filename: string): [string, string] {
  const index = filename.lastIndexOf(".");
  if (index > 0 && index < filename.length - 1) return [filename.slice(0, index), filename.slice(index + 1)];
  return [filename, ""];
}

export function extensionOf(filename: string) {
  return splitExtension(filename)[1].toLowerCase();
}

const imageExtensions = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "svg", "heic", "heif", "avif", "jfif",
]);
const archiveExtensions = new Set(["zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "xz", "zst", "cab", "iso"]);
const videoExtensions = new Set(["mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg"]);
const audioExtensions = new Set(["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "wma", "aiff"]);
/** Shown as text previews, same list as the Mac app plus a few Windows staples. */
const textExtensions = new Set([
  "txt", "json", "yml", "yaml", "swift", "js", "ts", "py", "log", "csv", "xml", "html", "css",
  "ini", "cfg", "conf", "toml", "ps1", "bat", "cmd", "sh", "rs", "c", "h", "cpp", "cs", "java", "go", "sql", "tsx", "jsx",
]);

/** Given the MIME type recorded at upload time, decides exactly like the
 * Rust side does for the post-upload clipboard and the local API; the
 * extension list is only for bucket objects, which have no recorded type. */
export function isImage(filename: string, mimeType?: string) {
  if (mimeType) return mimeType.startsWith("image/");
  return imageExtensions.has(extensionOf(filename));
}

export type FileKind = "image" | "pdf" | "archive" | "video" | "audio" | "text" | "other";

export function fileKind(filename: string): FileKind {
  const ext = extensionOf(filename);
  if (imageExtensions.has(ext)) return "image";
  if (ext === "pdf") return "pdf";
  if (archiveExtensions.has(ext)) return "archive";
  if (videoExtensions.has(ext)) return "video";
  if (audioExtensions.has(ext)) return "audio";
  if (textExtensions.has(ext) || ext === "md" || ext === "markdown") return "text";
  return "other";
}

export type PreviewKind = { kind: "image" } | { kind: "pdf" } | { kind: "text"; markdown: boolean } | { kind: "unsupported" };

export function previewKind(filename: string, mimeType?: string): PreviewKind {
  const ext = extensionOf(filename);
  if (ext === "md" || ext === "markdown") return { kind: "text", markdown: true };
  if (textExtensions.has(ext) || mimeType?.startsWith("text/")) return { kind: "text", markdown: false };
  // Browsers can't show HEIC or TIFF, so those fall back to the file icon.
  if (imageExtensions.has(ext) && !["heic", "heif", "tif", "tiff"].includes(ext)) return { kind: "image" };
  if (ext === "pdf" || mimeType === "application/pdf") return { kind: "pdf" };
  return { kind: "unsupported" };
}

export function formatOutput(
  url: string,
  mode: OutputMode,
  filename: string,
  customTemplate = "![{filename}]({url})",
  mimeType?: string,
) {
  switch (mode) {
    case "url":
      return url;
    case "markdown":
      return isImage(filename, mimeType) ? `![](${url})` : `[${filename}](${url})`;
    case "html":
      return isImage(filename, mimeType) ? `<img src="${url}" alt="">` : `<a href="${url}">${filename}</a>`;
    case "custom": {
      const [name, ext] = splitExtension(filename);
      return customTemplate
        .split("{url}").join(url)
        .split("{filename}").join(filename)
        .split("{name}").join(name)
        .split("{ext}").join(ext);
    }
  }
}

/** Binary units labeled KB, MB..., the way File Explorer shows sizes, so a
 * file reads the same here as in its Properties. */
export function formatBytes(bytes: number, locale: string) {
  const units = ["byte", "kilobyte", "megabyte", "gigabyte", "terabyte"] as const;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return new Intl.NumberFormat(locale, {
    style: "unit",
    unit: units[unit],
    unitDisplay: unit === 0 ? "long" : "short",
    maximumFractionDigits: unit === 0 || value >= 100 ? 0 : 1,
  }).format(value);
}

export function formatDateTime(millis: number, locale: string, style: "medium" | "full" = "medium") {
  return new Intl.DateTimeFormat(locale, {
    dateStyle: style,
    timeStyle: style === "full" ? "medium" : "short",
  }).format(new Date(millis));
}

export function formatTime(millis: number, locale: string) {
  return new Intl.DateTimeFormat(locale, { timeStyle: "short" }).format(new Date(millis));
}

function startOfDay(date: Date) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

export type DayBucket = { key: string; title: string };

/** "Today", "Yesterday", or the date, for grouping history. */
export function dayBucket(millis: number, locale: string, t: Translate): DayBucket {
  const now = new Date();
  const day = startOfDay(new Date(millis));
  const today = startOfDay(now);
  // Not today - 24 h: days around a DST change are 23 or 25 hours long.
  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1).getTime();
  if (day === today) return { key: "today", title: t("Today") };
  if (day === yesterday) return { key: "yesterday", title: t("Yesterday") };
  const date = new Date(day);
  const sameYear = date.getFullYear() === now.getFullYear();
  return {
    key: String(day),
    title: new Intl.DateTimeFormat(locale, sameYear ? { month: "long", day: "numeric" } : { dateStyle: "long" }).format(date),
  };
}

export function shortDay(millis: number, locale: string, t: Translate) {
  const bucket = dayBucket(millis, locale, t);
  if (bucket.key === "today" || bucket.key === "yesterday") return bucket.title;
  return new Intl.DateTimeFormat(locale, { month: "short", day: "numeric" }).format(new Date(millis));
}

export function providerName(preset: ProviderPreset, t: Translate) {
  switch (preset) {
    case "cloudflareR2":
      return "Cloudflare R2";
    case "amazonS3":
      return "Amazon S3";
    case "minIO":
      return "MinIO";
    case "backblazeB2":
      return "Backblaze B2";
    case "digitalOceanSpaces":
      return "DigitalOcean Spaces";
    case "customS3":
      return t("Other S3-Compatible");
  }
}

export function withoutScheme(url: string) {
  return url.replace(/^https?:\/\//, "");
}

// Bucket keys: S3 has no folders, only "/"-separated prefixes.

export function folderDisplayName(folder: string) {
  const trimmed = folder.endsWith("/") ? folder.slice(0, -1) : folder;
  return trimmed.split("/").pop() ?? "";
}

export function parentOfKey(key: string) {
  const index = key.lastIndexOf("/");
  return index < 0 ? "" : key.slice(0, index + 1);
}

export function parentOfFolder(folder: string) {
  return parentOfKey(folder.slice(0, -1));
}

export function nameOfKey(key: string) {
  return key.split("/").pop() ?? key;
}

/** Characters Foundation's `.urlPathAllowed` keeps, minus "+": S3 decodes a
 * "+" in a path as a space, so "a+b.png" would 404. Everything else in a key
 * segment is percent-encoded, same as the Rust side. */
const pathAllowed = /[A-Za-z0-9\-._~!$&'()*,;=:@]/;

export function encodeKeyPath(key: string) {
  return key
    .split("/")
    .map((segment) =>
      Array.from(segment)
        .map((char) =>
          pathAllowed.test(char)
            ? char
            : Array.from(new TextEncoder().encode(char))
                .map((byte) => `%${byte.toString(16).toUpperCase().padStart(2, "0")}`)
                .join(""),
        )
        .join(""),
    )
    .join("/");
}

/** publicURL = publicBaseURL + objectKey; a bare domain is treated as HTTPS. */
export function resolvePublicUrl(baseURL: string, key: string) {
  let base = baseURL.trim();
  if (!base.includes("://")) base = `https://${base}`;
  base = base.replace(/\/+$/, "");
  return `${base}/${encodeKeyPath(key)}`;
}

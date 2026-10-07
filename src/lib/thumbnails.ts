// The thumbnail folder rules of src-tauri/src/thumbnails (shared with the
// Mac and mobile apps), for the destination form to check as it's typed.

import type { Translate } from "./i18n";

export const defaultThumbnailPrefix = ".aktar/thumbnails/";

/** `raw` as a folder: trimmed, without leading slashes, ending in one; null
 * when nothing is left. */
export function normalizedThumbnailPrefix(raw: string): string | null {
  const prefix = raw.trim().replace(/^\/+/, "").replace(/\/+$/, "");
  return prefix ? `${prefix}/` : null;
}

/** Why `raw` can't be the thumbnail folder, or null when it can. */
export function thumbnailPrefixProblem(raw: string, t: Translate): string | null {
  const prefix = normalizedThumbnailPrefix(raw);
  if (!prefix) return t("Enter a folder for the thumbnails.");
  const segments = prefix.slice(0, -1).split("/");
  if (/[\u0000-\u001f\u007f\\:]/.test(prefix) || segments.some((segment) => segment === "" || segment === "." || segment === "..")) {
    return t(
      "“{0}” can’t be used as a name in the bucket. Leave out a “/” at the start, “.” and “..” as folder names, empty folder names (“//”), “\\” and “:”, and control characters.",
      prefix,
    );
  }
  if (prefix.startsWith("tmp/")) return t("The thumbnail folder can’t be inside tmp/, where files expire.");
  return null;
}

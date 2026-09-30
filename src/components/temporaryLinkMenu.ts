import { api, errorMessage, temporaryLinkDurations, type UploadRecord } from "../lib/api";
import { temporaryLinkTitle } from "../lib/format";
import type { Translate } from "../lib/i18n";
import type { MenuEntry } from "./Dialogs";

/** "Copy Temporary Link" for an upload in history: a fresh presigned link,
 * which works even when the bucket is private or the one copied at upload
 * time has run out. */
export function temporaryLinkMenu(record: UploadRecord, t: Translate, onError: (message: string) => void): MenuEntry {
  return {
    submenu: t("Copy Temporary Link"),
    items: temporaryLinkDurations.map((seconds) => ({
      label: temporaryLinkTitle(seconds, t),
      onClick: () => {
        api
          .recordTemporaryLink(record.id, seconds)
          .then((link) => api.copyText(link))
          .catch((error) => onError(errorMessage(error)));
      },
    })),
  };
}

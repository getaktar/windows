import { api, errorMessage, type DestinationConfig, type UploadRecord } from "../lib/api";
import type { Translate } from "../lib/i18n";
import { canCreateShortLink } from "../lib/shortLinks";
import type { MenuEntry } from "./Dialogs";

/** Copy Short Link, Copy Original Link (or Copy URL without one), Create
 * Short Link and Delete Short Link for an upload's menus. Create copies the
 * new link, or says why it couldn't in a notification. */
export function shortLinkMenu(
  record: UploadRecord,
  destinations: DestinationConfig[],
  t: Translate,
  onError: (message: string) => void,
): MenuEntry[] {
  const items: MenuEntry[] = record.shortUrl
    ? [
        { label: t("Copy Short Link"), onClick: () => api.copyText(record.shortUrl ?? record.publicUrl) },
        { label: t("Copy Original Link"), onClick: () => api.copyText(record.publicUrl) },
      ]
    : [{ label: t("Copy URL"), onClick: () => api.copyText(record.publicUrl) }];
  if (canCreateShortLink(record, destinations)) {
    items.push({ label: t("Create Short Link"), onClick: () => api.retryShortLink(record.id) });
  }
  const linkId = record.shortLinkId;
  if (linkId) {
    items.push({ label: t("Delete Short Link"), onClick: () => api.deleteShortLink(linkId).catch((error) => onError(errorMessage(error))) });
  }
  return items;
}

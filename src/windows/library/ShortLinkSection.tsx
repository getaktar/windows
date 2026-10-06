import { Badge, Button, Text, Tooltip } from "@fluentui/react-components";
import { CheckmarkRegular, CopyRegular, DeleteRegular } from "@fluentui/react-icons";
import { useCallback, useEffect, useState } from "react";

import { api, errorMessage, events, type ShortLink, type ShortLinkInfo, type ShortLinkStatus, type UploadRecord } from "../../lib/api";
import { useFlag, useTauriEvent } from "../../lib/hooks";
import { useI18n } from "../../lib/i18n";
import { relativeTime } from "../../lib/shortLinks";

/** The short links of an upload in its details: the active one with its
 * clicks (fetched when the details open, and cached), Create Short Link
 * when there's none, and older ones with their status. */
export function ShortLinkSection({ record }: { record: UploadRecord }) {
  const { t, locale } = useI18n();
  const [info, setInfo] = useState<ShortLinkInfo | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, flashCopied] = useFlag();

  const load = useCallback(
    (refresh: boolean) =>
      api
        .shortLinkInfo(record.id, refresh)
        .then(setInfo)
        .catch(() => {}),
    [record.id],
  );
  useEffect(() => {
    setInfo(null);
    setError(null);
    // Stats are fetched once the links show.
    load(false).then(() => load(true));
  }, [load]);
  useTauriEvent(events.historyChanged, () => load(false));

  if (!info) return null;
  const active = info.links.find((link) => link.id === info.activeId) ?? null;
  const older = info.links.filter((link) => link.id !== info.activeId && link.status !== "deleted");
  if (!active && older.length === 0 && !info.canCreate && !error) return null;

  const run = async (action: () => Promise<void>) => {
    setWorking(true);
    setError(null);
    try {
      await action();
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setWorking(false);
      load(false);
    }
  };

  /** "Clicks: 12 · Last clicked 2 hours ago". */
  const stats = (link: ShortLink) => {
    const parts: string[] = [];
    if (link.clicks !== null) parts.push(t("Clicks: {0}", link.clicks));
    if (link.lastClickAt !== null) parts.push(t("Last clicked {0}", relativeTime(link.lastClickAt, locale)));
    return parts.length > 0 ? parts.join(" · ") : null;
  };

  return (
    <section className="detail-section">
      <Text weight="semibold" className="secondary">
        {t("Short Link")}
      </Text>
      {active && (
        <>
          <div className="link-line">
            <Text className="ellipsis selectable" title={active.shortUrl}>
              {active.shortUrl}
            </Text>
            <Tooltip content={t("Copy Short Link")} relationship="label">
              <Button
                appearance="subtle"
                size="small"
                icon={copied ? <CheckmarkRegular /> : <CopyRegular />}
                onClick={() => {
                  api.copyText(active.shortUrl);
                  flashCopied();
                }}
              />
            </Tooltip>
            <Tooltip content={t("Delete Short Link")} relationship="label">
              <Button appearance="subtle" size="small" icon={<DeleteRegular />} disabled={working} onClick={() => run(() => api.deleteShortLink(active.id))} />
            </Tooltip>
          </div>
          {info.hasStats && stats(active) && (
            <Text size={200} className="secondary">
              {stats(active)}
            </Text>
          )}
        </>
      )}
      {info.canCreate && (
        <div>
          <Button size="small" disabled={working} onClick={() => run(() => api.createShortLink(record.id))}>
            {working ? t("Creating…") : t("Create Short Link")}
          </Button>
        </div>
      )}
      {error && (
        <Text size={200} className="text-error">
          {error}
        </Text>
      )}
      {older.length > 0 && (
        <div className="short-link-older">
          <Text size={200} weight="semibold" className="secondary">
            {t("Earlier Short Links")}
          </Text>
          {older.map((link) => (
            <div key={link.id} className="link-line">
              <Text size={200} className="ellipsis selectable" title={link.shortUrl}>
                {link.shortUrl}
              </Text>
              <ShortLinkStatusBadge status={link.displayStatus} />
              <Tooltip content={t("Copy Short Link")} relationship="label">
                <Button appearance="subtle" size="small" icon={<CopyRegular />} onClick={() => api.copyText(link.shortUrl)} />
              </Tooltip>
              <Tooltip content={t("Delete Short Link")} relationship="label">
                <Button appearance="subtle" size="small" icon={<DeleteRegular />} disabled={working} onClick={() => run(() => api.deleteShortLink(link.id))} />
              </Tooltip>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

/** Orphaned, unknown and expired short links say so. */
function ShortLinkStatusBadge({ status }: { status: ShortLinkStatus }) {
  const { t } = useI18n();
  const badge = (label: string, color: "informative" | "warning", help: string) => (
    <Badge className="short-link-badge" appearance="outline" color={color} size="small" title={help}>
      {label}
    </Badge>
  );
  switch (status) {
    case "active":
      return null;
    case "expired":
      return badge(t("Expired"), "informative", t("The upload or its temporary link expired."));
    case "deleted":
      return badge(t("Deleted"), "informative", t("Deleted at the link shortener."));
    case "orphaned":
      return badge(t("May still exist"), "warning", t("Short link may still exist"));
    case "unknown":
      return badge(t("Not updated"), "warning", t("Its target couldn’t be updated, so it may still point to the old location."));
  }
}

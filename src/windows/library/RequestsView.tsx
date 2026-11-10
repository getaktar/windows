import { Button, Dialog, DialogActions, DialogBody, DialogContent, DialogSurface, DialogTitle, Text } from "@fluentui/react-components";
import { AddRegular, ArrowSyncRegular, CopyRegular, DismissRegular, QrCodeRegular } from "@fluentui/react-icons";
import { useState } from "react";

import { RequestFilesDialog, RequestLink, requestSizeLabel } from "../../components/RequestFilesDialog";
import { api, errorMessage, type FileRequest } from "../../lib/api";
import { formatDateTime } from "../../lib/format";
import { useDestinations, useRequests } from "../../lib/hooks";
import { useI18n } from "../../lib/i18n";

/** Library > Requests: the file requests made on this PC, open first. */
export function RequestsView({ active }: { active: boolean }) {
  const { t, locale } = useI18n();
  const [requests] = useRequests();
  const [destinations] = useDestinations();
  const [creating, setCreating] = useState(false);
  const [checking, setChecking] = useState(false);
  const [shown, setShown] = useState<FileRequest | null>(null);
  const [error, setError] = useState<string | null>(null);

  const now = Date.now();
  const isOpen = (request: FileRequest) => !request.closedAt && Date.parse(request.expiresAt) > now;
  const sorted = [...requests].sort((a, b) => Number(isOpen(b)) - Number(isOpen(a)));

  const checkNow = () => {
    setChecking(true);
    api
      .checkRequests()
      .catch((error) => setError(errorMessage(error)))
      .finally(() => setChecking(false));
  };

  return (
    <section className="requests-view" hidden={!active}>
      <div className="requests-toolbar">
        <Text size={500} weight="semibold">
          {t("Requests")}
        </Text>
        <div className="inline-row">
          <Button icon={<ArrowSyncRegular />} disabled={checking} onClick={checkNow}>
            {t("Check Now")}
          </Button>
          <Button appearance="primary" icon={<AddRegular />} disabled={destinations.length === 0} onClick={() => setCreating(true)}>
            {t("Request Files…")}
          </Button>
        </div>
      </div>
      {error && (
        <Text size={200} className="text-error">
          {error}
        </Text>
      )}
      {sorted.length === 0 ? (
        <Text className="secondary requests-empty">
          {t("Make a link where anyone can send files straight to your bucket. They show up in your history.")}
        </Text>
      ) : (
        <div className="requests-list">
          {sorted.map((request) => {
            const open = isOpen(request);
            const destination = destinations.find((candidate) => candidate.id === request.destinationID);
            return (
              <div key={request.id} className="request-item">
                <div className="grow">
                  <Text weight="semibold" block>
                    {request.title || t("File Request")}
                  </Text>
                  <Text size={200} className="secondary" block>
                    {[
                      destination?.name,
                      request.received === 1 ? t("Received 1 file") : t("Received {0} files", request.received),
                      t("Up to {0} files, {1} each", request.slotCount, requestSizeLabel(request.maxBytes)),
                      open
                        ? t("Available until {0}", formatDateTime(Date.parse(request.expiresAt), locale))
                        : t("Closed"),
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </Text>
                </div>
                {open ? (
                  <>
                    <Button appearance="subtle" icon={<CopyRegular />} title={t("Copy Link")} aria-label={t("Copy Link")} onClick={() => api.copyText(request.link)} />
                    <Button appearance="subtle" icon={<QrCodeRegular />} title={t("Show QR Code")} aria-label={t("Show QR Code")} onClick={() => setShown(request)} />
                    <Button onClick={() => api.closeRequest(request.id).catch((error) => setError(errorMessage(error)))}>{t("Close Now")}</Button>
                  </>
                ) : (
                  request.closedAt && (
                    <Button appearance="subtle" icon={<DismissRegular />} title={t("Remove")} aria-label={t("Remove")} onClick={() => api.forgetRequest(request.id)} />
                  )
                )}
              </div>
            );
          })}
        </div>
      )}
      <RequestFilesDialog open={creating} onClose={() => setCreating(false)} />
      <Dialog open={shown !== null} onOpenChange={(_, data) => !data.open && setShown(null)}>
        <DialogSurface className="qr-surface">
          <DialogBody>
            <DialogTitle>{shown?.title || t("File Request")}</DialogTitle>
            <DialogContent className="qr-content">{shown && <RequestLink request={shown} />}</DialogContent>
            <DialogActions>
              <Button appearance="primary" onClick={() => setShown(null)}>
                {t("Done")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </section>
  );
}

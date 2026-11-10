import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Select,
  Spinner,
  Text,
  Textarea,
} from "@fluentui/react-components";
import { CheckmarkRegular, CopyRegular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";

import {
  api,
  errorMessage,
  requestDurations,
  requestSizeLimits,
  requestSlotCounts,
  type DestinationConfig,
  type FileRequest,
  type QrMatrix,
  type RequestError,
} from "../lib/api";
import { durationLabel } from "../lib/format";
import { useDestinations, useFlag } from "../lib/hooks";
import { useI18n, type Translate } from "../lib/i18n";
import { QrSvg } from "./QrCodeDialog";

/** "1 hour", "1 day", "7 days". */
export function requestDurationLabel(seconds: number, t: Translate) {
  return seconds === 3600 ? t("1 hour") : durationLabel(seconds / 86_400, t);
}

/** Decimal, as the page itself counts. */
export function requestSizeLabel(bytes: number) {
  return bytes >= 1_000_000_000 ? `${bytes / 1_000_000_000} GB` : `${bytes / 1_000_000} MB`;
}

function requestError(error: unknown): RequestError {
  if (error && typeof error === "object" && "kind" in error) return error as RequestError;
  return { kind: "failed", message: errorMessage(error) };
}

/** "Request Files" (docs/share-pages.md, part 4): a link where anyone can
 * send files straight into a destination's bucket until it expires. */
export function RequestFilesDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [destinationId, setDestinationId] = useState<string>("");
  const [title, setTitle] = useState("");
  const [note, setNote] = useState("");
  const [seconds, setSeconds] = useState(86_400);
  const [slotCount, setSlotCount] = useState(10);
  const [maxBytes, setMaxBytes] = useState(2_000_000_000);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<RequestError | null>(null);
  const [made, setMade] = useState<FileRequest | null>(null);

  useEffect(() => {
    if (!open) return;
    setTitle("");
    setNote("");
    setSeconds(86_400);
    setSlotCount(10);
    setMaxBytes(2_000_000_000);
    setError(null);
    setMade(null);
    setBusy(false);
    setDestinationId((destinations.find((destination) => destination.isDefault) ?? destinations[0])?.id ?? "");
  }, [open]);

  const destination: DestinationConfig | undefined = destinations.find((candidate) => candidate.id === destinationId);
  const conditional = destination?.preset === "amazonS3" || destination?.preset === "cloudflareR2";

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      const request = await api.createRequest({ destinationId: destinationId || null, title, note, seconds, slotCount, maxBytes });
      setMade(request);
      await api.copyText(request.link);
    } catch (error) {
      setError(requestError(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className="request-surface">
        <DialogBody>
          <DialogTitle>{t("Request Files")}</DialogTitle>
          <DialogContent className="request-content">
            {made ? (
              <RequestLink request={made} copiedNote />
            ) : (
              <>
                <Field label={t("Destination")}>
                  <Select value={destinationId} onChange={(_, data) => setDestinationId(data.value)}>
                    {destinations.map((candidate) => (
                      <option key={candidate.id} value={candidate.id}>
                        {candidate.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                {destination && !conditional && (
                  <Text size={200} className="secondary">
                    {t("On this provider a request link is meant for one sender.")}
                  </Text>
                )}
                <Field label={t("Title")}>
                  <Input value={title} maxLength={100} placeholder={t("Optional")} onChange={(_, data) => setTitle(data.value)} />
                </Field>
                <Field label={t("Note")}>
                  <Textarea value={note} maxLength={1000} placeholder={t("Optional")} resize="vertical" onChange={(_, data) => setNote(data.value)} />
                </Field>
                <div className="request-row">
                  <Field label={t("Valid for")}>
                    <Select value={String(seconds)} onChange={(_, data) => setSeconds(Number(data.value))}>
                      {requestDurations.map((value) => (
                        <option key={value} value={value}>
                          {requestDurationLabel(value, t)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("Up to")}>
                    <Select value={String(slotCount)} onChange={(_, data) => setSlotCount(Number(data.value))}>
                      {requestSlotCounts.map((value) => (
                        <option key={value} value={value}>
                          {value === 1 ? t("1 file") : t("{0} files", value)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  <Field label={t("Size limit per file")}>
                    <Select value={String(maxBytes)} onChange={(_, data) => setMaxBytes(Number(data.value))}>
                      {requestSizeLimits.map((value) => (
                        <option key={value} value={value}>
                          {requestSizeLabel(value)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                </div>
                {error && (
                  <MessageBar intent="error" layout="multiline">
                    <MessageBarBody>
                      {error.message}
                      {error.kind === "cors" && <pre className="request-rule selectable">{error.rule}</pre>}
                    </MessageBarBody>
                  </MessageBar>
                )}
                <Text size={200} className="secondary">
                  {t("Anyone with the link can send files until it expires.")}{" "}
                  {t("The size limit is checked by the page, not the bucket. Files sent while Aktar isn’t running wait in the bucket until it runs.")}
                </Text>
              </>
            )}
          </DialogContent>
          <DialogActions>
            {made ? (
              <Button appearance="primary" onClick={onClose}>
                {t("Done")}
              </Button>
            ) : (
              <>
                {error?.kind === "cors" && (
                  <Button onClick={() => api.copyText(error.rule)}>
                    {t("Copy Rule")}
                  </Button>
                )}
                <Button appearance="secondary" onClick={onClose}>
                  {t("Cancel")}
                </Button>
                <Button appearance="primary" disabled={busy || !destination} icon={busy ? <Spinner size="tiny" /> : undefined} onClick={create}>
                  {t("Create Link")}
                </Button>
              </>
            )}
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

/** A request's link, with Copy and its QR code. */
export function RequestLink({ request, copiedNote = false }: { request: FileRequest; copiedNote?: boolean }) {
  const { t } = useI18n();
  const [matrix, setMatrix] = useState<QrMatrix | null>(null);
  const [copied, flashCopied] = useFlag();
  useEffect(() => {
    let current = true;
    api
      .qrCode(request.link)
      .then((matrix) => current && setMatrix(matrix))
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [request.link]);
  return (
    <div className="request-link">
      {copiedNote && (
        <Text size={200} className="secondary">
          {t("Link copied. Anyone who has it can send files until it expires.")}
        </Text>
      )}
      <div className="qr-code">{matrix ? <QrSvg matrix={matrix} /> : <Spinner />}</div>
      <div className="inline-row">
        <Input className="grow" readOnly value={request.link} />
        <Button
          icon={copied ? <CheckmarkRegular /> : <CopyRegular />}
          onClick={() => {
            api.copyText(request.link);
            flashCopied();
          }}
        >
          {copied ? t("Copied") : t("Copy")}
        </Button>
      </div>
    </div>
  );
}

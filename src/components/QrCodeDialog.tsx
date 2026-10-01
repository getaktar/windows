import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Spinner,
  Text,
} from "@fluentui/react-components";
import { CheckmarkRegular, CopyRegular, SaveRegular } from "@fluentui/react-icons";
import { save } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

import { api, errorMessage, type DestinationConfig, type QrMatrix, type UploadRecord } from "../lib/api";
import { splitExtension } from "../lib/format";
import { useFlag } from "../lib/hooks";
import { useI18n } from "../lib/i18n";

/** The link "Show QR Code" encodes: the one that would be copied, so a
 * fresh temporary link of the same duration when the upload's destination
 * is set to temporary links, and the public URL otherwise (or when the
 * destination is gone). */
async function linkFor(record: UploadRecord, destinations: DestinationConfig[]) {
  const destination = destinations.find((candidate) => candidate.id === record.destinationId);
  if (destination?.temporaryLink) {
    try {
      return await api.recordTemporaryLink(record.id, destination.temporaryLink);
    } catch {
      // Signing only fails without the keys: the public URL it is.
    }
  }
  return record.publicUrl;
}

/** "https://img.example.com/2026/…/0b4e.png": the start and end of a long
 * link, which say the most about it. */
export function middleTruncated(text: string, length = 56) {
  if (text.length <= length) return text;
  const head = Math.ceil((length - 1) / 2);
  return `${text.slice(0, head)}…${text.slice(text.length - (length - 1 - head))}`;
}

/** A QR code for an upload's link, with Copy Image and Save Image. Open
 * while `record` is set. */
export function QrCodeDialog({
  record,
  destinations,
  onClose,
}: {
  record: UploadRecord | null;
  destinations: DestinationConfig[];
  onClose: () => void;
}) {
  const { t } = useI18n();
  const [link, setLink] = useState<string | null>(null);
  const [matrix, setMatrix] = useState<QrMatrix | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, flashCopied] = useFlag();

  useEffect(() => {
    setLink(null);
    setMatrix(null);
    setError(null);
    if (!record) return;
    let current = true;
    linkFor(record, destinations)
      .then(async (link) => {
        const matrix = await api.qrCode(link);
        if (!current) return;
        setLink(link);
        setMatrix(matrix);
      })
      .catch((error) => current && setError(errorMessage(error)));
    return () => {
      current = false;
    };
    // The link is made once per opening, not again when destinations reload.
  }, [record?.id]);

  const copyImage = () => {
    if (!link) return;
    api
      .copyQrImage(link)
      .then(flashCopied)
      .catch((error) => setError(errorMessage(error)));
  };

  const saveImage = async () => {
    if (!link || !record) return;
    // The save dialog takes focus; that mustn't close the panel.
    await api.setPanelShowingDialog(true);
    try {
      const path = await save({
        defaultPath: `${splitExtension(record.localFilename)[0] || "link"} QR.png`,
        filters: [{ name: "PNG", extensions: ["png"] }],
      });
      if (path) await api.saveQrImage(link, path);
    } catch (error) {
      setError(errorMessage(error));
    } finally {
      await api.setPanelShowingDialog(false);
    }
  };

  return (
    <Dialog open={record !== null} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className="qr-surface">
        <DialogBody>
          <DialogTitle>{t("QR Code")}</DialogTitle>
          <DialogContent className="qr-content">
            <div className="qr-code">{matrix ? <QrSvg matrix={matrix} /> : !error && <Spinner />}</div>
            <Text size={200} className="secondary">
              {t("Scan to open the link")}
            </Text>
            {link && (
              <Text size={200} className="qr-link selectable" title={link}>
                {middleTruncated(link)}
              </Text>
            )}
            {error && (
              <Text size={200} className="text-error">
                {error}
              </Text>
            )}
          </DialogContent>
          <DialogActions fluid className="qr-actions">
            <Button icon={copied ? <CheckmarkRegular /> : <CopyRegular />} disabled={!matrix} onClick={copyImage}>
              {copied ? t("Copied") : t("Copy Image")}
            </Button>
            <Button icon={<SaveRegular />} disabled={!matrix} onClick={saveImage}>
              {t("Save Image…")}
            </Button>
            <Button appearance="primary" onClick={onClose}>
              {t("Done")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

/** One square per dark module on white, with the four-module quiet zone,
 * drawn as vectors with crisp edges so it stays sharp at any scale and in
 * dark mode. */
function QrSvg({ matrix }: { matrix: QrMatrix }) {
  const quiet = 4;
  const side = matrix.size + quiet * 2;
  let path = "";
  for (let row = 0; row < matrix.size; row++) {
    for (let column = 0; column < matrix.size; column++) {
      if (matrix.modules[row * matrix.size + column] === "1") path += `M${column + quiet} ${row + quiet}h1v1h-1z`;
    }
  }
  return (
    <svg viewBox={`0 0 ${side} ${side}`} shapeRendering="crispEdges" role="img" aria-label="QR">
      <rect width={side} height={side} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}

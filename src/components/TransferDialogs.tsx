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
  Spinner,
  Text,
} from "@fluentui/react-components";
import { CameraRegular, CopyRegular } from "@fluentui/react-icons";
import jsQR from "jsqr";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type RefObject } from "react";

import {
  api,
  errorMessage,
  type ConnectionResult,
  type DestinationConfig,
  type QrMatrix,
  type TransferPayload,
  type TransferShare,
} from "../lib/api";
import { useI18n, type Translate } from "../lib/i18n";
import { ConnectionTestResult } from "./ConnectionTestResult";
import { ConfirmDialog } from "./Dialogs";
import { QrSvg } from "./QrCodeDialog";

/** How long "Share to Another Device" stays open with its code. */
const SHARE_LIFETIME_MS = 10 * 60 * 1000;
/** Crockford base32, as in src-tauri/src/transfer.rs. */
const CODE_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

const CODE_LENGTH = 12;

/** What was typed, as the 12 characters of a transfer code, or null. Rust
 * checks it again; this only enables Continue. */
function normalizeCode(input: string) {
  const code = input.toUpperCase().replace(/[\s-]/g, "").replace(/O/g, "0").replace(/[IL]/g, "1");
  return code.length === CODE_LENGTH && [...code].every((character) => CODE_ALPHABET.includes(character)) ? code : null;
}

/** The letters and digits in `text`, uppercased: what the code field
 * keeps of anything typed or pasted. */
function codeCharacters(text: string) {
  return text.toUpperCase().replace(/[^0-9A-Z]/g, "");
}

/** "K7P2QX9M4TRW" as "K7P2-QX9M-4TRW", and a partial code the same way,
 * without a hyphen at the end. */
function formatCode(characters: string) {
  return characters.match(/.{1,4}/g)?.join("-") ?? "";
}

/** Where the caret goes in `formatCode` output to sit after `count` code
 * characters. */
function caretAfter(count: number) {
  return count + Math.min(2, Math.max(0, Math.floor((count - 1) / 4)));
}

/** An endpoint as written, ignoring case, a trailing slash and a missing
 * https:// scheme. */
function endpointKey(endpoint: string) {
  return endpoint.trim().toLowerCase().replace(/^https:\/\//, "").replace(/\/+$/, "");
}

/** Whether `imported` uploads somewhere else than `existing` does now. */
function uploadsElsewhere(imported: DestinationConfig, existing: DestinationConfig) {
  return endpointKey(imported.endpoint) !== endpointKey(existing.endpoint) || imported.bucket.trim() !== existing.bucket.trim();
}

/** The text for a `TransferError` from Rust, or the message of anything else. */
function transferErrorMessage(error: unknown, t: Translate) {
  switch (error) {
    case "wrongCode":
      return t("That code doesn’t match. Check it and try again.");
    case "newerVersion":
      return t("This was shared from a newer version of Aktar. Update Aktar and try again.");
    case "notTransfer":
      return t("This isn’t an Aktar transfer link.");
    default:
      return errorMessage(error);
  }
}

function cameraErrorMessage(error: unknown, t: Translate) {
  const name = error instanceof DOMException ? error.name : "";
  // NotReadableError is also what Windows' camera privacy setting gives.
  if (name === "NotAllowedError" || name === "SecurityError" || name === "NotReadableError") {
    return t("Camera access is off. Turn it on in Settings, or paste the transfer link instead.");
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return t("No camera found. Paste the transfer link instead.");
  }
  return errorMessage(error);
}

// MARK: - Share

export interface ShareRequest extends TransferShare {
  name: string;
}

/** "Share to Another Device": the destination's QR code and transfer code,
 * open while `share` is set. It closes itself after 10 minutes; the code
 * is gone once it's closed. No Copy Image or Save Image: the picture holds
 * the keys, and the window is kept out of screenshots, recordings and
 * screen sharing while it's open. */
export function ShareDestinationDialog({ share, onClose }: { share: ShareRequest | null; onClose: () => void }) {
  const { t } = useI18n();
  const [matrix, setMatrix] = useState<QrMatrix | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setMatrix(null);
    setError(null);
    setCopied(false);
    if (!share) return;
    let current = true;
    api
      .qrCode(share.link)
      .then((matrix) => current && setMatrix(matrix))
      .catch((error) => current && setError(errorMessage(error)));
    const timer = window.setTimeout(onClose, SHARE_LIFETIME_MS);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [share]);

  // Keeps the code and the keys out of screenshots, recordings and screen
  // sharing for as long as they're on screen.
  const isOpen = share !== null;
  useEffect(() => {
    if (!isOpen) return;
    const appWindow = getCurrentWindow();
    appWindow.setContentProtected(true).catch(() => {});
    return () => {
      appWindow.setContentProtected(false).catch(() => {});
    };
  }, [isOpen]);

  const copyLink = () => {
    if (!share) return;
    api
      .copyText(share.link)
      .then(() => setCopied(true))
      .catch((error) => setError(errorMessage(error)));
  };

  return (
    <Dialog open={share !== null} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className="qr-surface">
        <DialogBody>
          <DialogTitle>{share?.name}</DialogTitle>
          <DialogContent className="qr-content">
            <div className="qr-code">{matrix ? <QrSvg matrix={matrix} /> : !error && <Spinner />}</div>
            <Text size={200} className="secondary">
              {t("Transfer Code")}
            </Text>
            <Text className="transfer-code selectable">{share?.code}</Text>
            <Text size={200}>{t("Scan this QR code with Aktar on your other device, then enter the transfer code there.")}</Text>
            <Text size={200} className="text-warning">
              {t("This QR code contains your access keys. Only scan it with your own devices.")}
            </Text>
            {copied && (
              <Text size={200} className="text-success">
                {t("Link copied. Share the transfer code separately.")}
              </Text>
            )}
            {error && (
              <Text size={200} className="text-error">
                {error}
              </Text>
            )}
            <Text size={100} className="secondary">
              {t("For your security, this closes after 10 minutes.")}
            </Text>
          </DialogContent>
          <DialogActions fluid className="qr-actions">
            <Button icon={<CopyRegular />} onClick={copyLink}>
              {t("Copy Transfer Link")}
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

// MARK: - Import

type Stage = "link" | "code" | "duplicate" | "result";

/** The transfer code field: formats what's typed or pasted as
 * XXXX-XXXX-XXXX while keeping the caret after the same character, and
 * deletes across a hyphen as if it weren't there. */
function TransferCodeInput({
  inputRef,
  value,
  onChange,
}: {
  inputRef: RefObject<HTMLInputElement | null>;
  value: string;
  onChange: (value: string) => void;
}) {
  /** Where the caret goes once the formatted value is rendered. */
  const pendingCaret = useRef<number | null>(null);
  /** Renders again when an edit leaves the value as it was (an extra
   * character on a full code, or a lone hyphen deleted), so the input
   * shows the value again with the caret in place. */
  const [, rerender] = useState(0);

  useLayoutEffect(() => {
    const caret = pendingCaret.current;
    pendingCaret.current = null;
    if (caret !== null && document.activeElement === inputRef.current) inputRef.current?.setSelectionRange(caret, caret);
  });

  const apply = (characters: string, charactersBeforeCaret: number) => {
    const next = formatCode(characters);
    pendingCaret.current = caretAfter(Math.max(0, Math.min(charactersBeforeCaret, characters.length)));
    if (next === value) rerender((count) => count + 1);
    else onChange(next);
  };

  const handleChange = (raw: string, caret: number) => {
    const characters = codeCharacters(raw);
    const before = codeCharacters(raw.slice(0, caret)).length;
    const current = codeCharacters(value);
    if (characters.length > CODE_LENGTH && current.length === CODE_LENGTH) {
      // Typing into a full code changes nothing.
      apply(current, before - (characters.length - current.length));
      return;
    }
    apply(characters.slice(0, CODE_LENGTH), before);
  };

  // A hyphen is only formatting: Backspace right after one (or Delete
  // right before one) removes the character on its other side.
  const handleKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    const input = event.currentTarget;
    const start = input.selectionStart ?? 0;
    if (start !== input.selectionEnd || event.ctrlKey || event.metaKey || event.altKey) return;
    let removeAt = -1;
    if (event.key === "Backspace" && value[start - 1] === "-") removeAt = start - 2;
    if (event.key === "Delete" && value[start] === "-") removeAt = start + 1;
    if (removeAt < 0 || removeAt >= value.length) return;
    event.preventDefault();
    const before = codeCharacters(value.slice(0, event.key === "Backspace" ? removeAt : start)).length;
    apply(codeCharacters(value.slice(0, removeAt) + value.slice(removeAt + 1)), before);
  };

  return (
    <Input
      ref={inputRef}
      className="transfer-code-input"
      value={value}
      placeholder="XXXX-XXXX-XXXX"
      autoComplete="off"
      spellCheck={false}
      onKeyDown={handleKeyDown}
      onChange={(event, data) => handleChange(data.value, event.currentTarget.selectionStart ?? data.value.length)}
    />
  );
}

/** "Import from Another Device": the link (pasted, or scanned with the
 * camera), then the transfer code, then what to do when the destination
 * is already here. The destination is then saved as it came, and the last
 * step says so and tests the connection; it stays saved whatever the test
 * finds. `initialLink` comes from an aktar://import link. */
export function ImportDestinationDialog({
  open,
  initialLink,
  onClose,
  onDone,
  onEdit,
}: {
  open: boolean;
  initialLink?: string | null;
  /** Closed before anything was saved. */
  onClose: () => void;
  /** "Done" on the last step, or closing it. */
  onDone: (destination: DestinationConfig) => void;
  /** "Edit" on the last step: the destination form for what was saved. */
  onEdit: (destination: DestinationConfig) => void;
}) {
  const { t } = useI18n();
  const [stage, setStage] = useState<Stage>("link");
  const [link, setLink] = useState("");
  const [linkError, setLinkError] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [codeError, setCodeError] = useState<string | null>(null);
  const [isWorking, setIsWorking] = useState(false);
  const [payload, setPayload] = useState<TransferPayload | null>(null);
  const [existingName, setExistingName] = useState("");
  /** Set when "Update Existing" would make it upload somewhere else. */
  const [movesUploads, setMovesUploads] = useState(false);
  /** The destination as saved, and whether it replaced one with its ID. */
  const [saved, setSaved] = useState<{ destination: DestinationConfig; updated: boolean } | null>(null);
  const [isTesting, setIsTesting] = useState(false);
  const [testResult, setTestResult] = useState<ConnectionResult | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [cameraError, setCameraError] = useState<string | null>(null);
  const video = useRef<HTMLVideoElement>(null);
  const stream = useRef<MediaStream | null>(null);
  /** Bumped when the camera is stopped, so a start still on its way is
   * dropped when it arrives. */
  const cameraGeneration = useRef(0);
  /** Bumped when the dialog opens or closes, so a test still running from
   * before doesn't show up in the next import. */
  const session = useRef(0);
  const codeInput = useRef<HTMLInputElement>(null);
  const doneButton = useRef<HTMLButtonElement>(null);

  const stopCamera = () => {
    cameraGeneration.current++;
    stream.current?.getTracks().forEach((track) => track.stop());
    stream.current = null;
    if (video.current) video.current.srcObject = null;
    setScanning(false);
  };

  useEffect(() => {
    session.current++;
    if (!open) {
      stopCamera();
      return;
    }
    setStage("link");
    setLink(initialLink ?? "");
    setLinkError(null);
    setCode("");
    setCodeError(null);
    setIsWorking(false);
    setPayload(null);
    setSaved(null);
    setIsTesting(false);
    setTestResult(null);
    setTestError(null);
    setCameraError(null);
    if (initialLink) submitLink(initialLink);
  }, [open, initialLink]);

  // The camera never outlives the dialog.
  useEffect(() => () => stopCamera(), []);

  useEffect(() => {
    if (stage === "code") window.setTimeout(() => codeInput.current?.focus(), 0);
    // The code field it replaces had focus; without it, Esc and Enter
    // would go nowhere.
    if (stage === "result") window.setTimeout(() => doneButton.current?.focus(), 0);
  }, [stage]);

  const submitLink = async (value: string) => {
    setLinkError(null);
    try {
      await api.checkTransferLink(value);
      setLink(value.trim());
      setStage("code");
    } catch (error) {
      setLinkError(transferErrorMessage(error, t));
    }
  };

  const startCamera = async () => {
    setCameraError(null);
    setLinkError(null);
    const generation = ++cameraGeneration.current;
    try {
      if (!navigator.mediaDevices?.getUserMedia) throw new DOMException("", "NotFoundError");
      const devices = await navigator.mediaDevices.enumerateDevices();
      if (!devices.some((device) => device.kind === "videoinput")) throw new DOMException("", "NotFoundError");
      const media = await navigator.mediaDevices.getUserMedia({
        video: { facingMode: "environment", width: { ideal: 1280 }, height: { ideal: 720 } },
        audio: false,
      });
      if (generation !== cameraGeneration.current) {
        media.getTracks().forEach((track) => track.stop());
        return;
      }
      stream.current = media;
      setScanning(true);
    } catch (error) {
      if (generation === cameraGeneration.current) setCameraError(cameraErrorMessage(error, t));
    }
  };

  // Looks for a QR code a few times a second while the camera is on.
  useEffect(() => {
    const element = video.current;
    if (!scanning || !element || !stream.current) return;
    element.srcObject = stream.current;
    element.play().catch(() => {});
    const canvas = document.createElement("canvas");
    const context = canvas.getContext("2d", { willReadFrequently: true });
    let timer = 0;
    let checking = false;
    let rejected = "";
    const scan = () => {
      timer = window.setTimeout(scan, 150);
      if (checking || !context || element.readyState < element.HAVE_ENOUGH_DATA || !element.videoWidth) return;
      // A transfer QR code is dense: keep enough pixels per module.
      const scale = Math.min(1, 1280 / element.videoWidth);
      canvas.width = Math.round(element.videoWidth * scale);
      canvas.height = Math.round(element.videoHeight * scale);
      context.drawImage(element, 0, 0, canvas.width, canvas.height);
      const found = jsQR(context.getImageData(0, 0, canvas.width, canvas.height).data, canvas.width, canvas.height, {
        inversionAttempts: "dontInvert",
      });
      if (!found?.data || found.data === rejected) return;
      checking = true;
      api
        .checkTransferLink(found.data)
        .then(() => {
          stopCamera();
          setLink(found.data.trim());
          setStage("code");
        })
        .catch((error) => {
          // Some other QR code: say so, and keep looking.
          rejected = found.data;
          setCameraError(transferErrorMessage(error, t));
        })
        .finally(() => {
          checking = false;
        });
    };
    scan();
    return () => window.clearTimeout(timer);
  }, [scanning]);

  const submitCode = async () => {
    if (!normalizeCode(code) || isWorking) return;
    setIsWorking(true);
    setCodeError(null);
    try {
      const opened = await api.openTransfer(link, code);
      const destinations = await api.listDestinations();
      const existing = destinations.find((destination) => destination.id.toUpperCase() === opened.destination.id.toUpperCase());
      setPayload(opened);
      if (existing) {
        setExistingName(existing.name);
        setMovesUploads(uploadsElsewhere(opened.destination, existing));
        setStage("duplicate");
      } else {
        await save(opened, "new");
      }
    } catch (error) {
      setCodeError(transferErrorMessage(error, t));
      window.setTimeout(() => codeInput.current?.select(), 0);
    } finally {
      setIsWorking(false);
    }
  };

  /** Saves the destination as it came (a copy under a new ID for "Add as
   * Copy"), then tests its connection. */
  const save = async (opened: TransferPayload, mode: "new" | "update" | "copy") => {
    const config =
      mode === "copy"
        ? { ...opened.destination, id: crypto.randomUUID().toUpperCase(), name: t("{0} Copy", opened.destination.name) }
        : opened.destination;
    const destination = await api.importDestination(config, opened.credentials, opened.customTemplate);
    setSaved({ destination, updated: mode === "update" });
    setStage("result");
    testConnection(destination);
  };

  /** From the duplicate question: errors go back to the code step. */
  const saveDuplicate = async (mode: "update" | "copy") => {
    if (!payload) return;
    setStage("code");
    setIsWorking(true);
    try {
      await save(payload, mode);
    } catch (error) {
      setCodeError(transferErrorMessage(error, t));
    } finally {
      setIsWorking(false);
    }
  };

  const testConnection = async (destination: DestinationConfig) => {
    const current = session.current;
    setIsTesting(true);
    setTestResult(null);
    setTestError(null);
    try {
      const result = await api.testConnection(destination, null);
      if (current === session.current) setTestResult(result);
    } catch (error) {
      if (current === session.current) setTestError(errorMessage(error));
    } finally {
      if (current === session.current) setIsTesting(false);
    }
  };

  const close = () => {
    stopCamera();
    if (saved) onDone(saved.destination);
    else onClose();
  };

  return (
    <>
      <Dialog open={open && stage !== "duplicate"} onOpenChange={(_, data) => !data.open && close()}>
        <DialogSurface className="import-surface">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              if (stage === "link" && link.trim()) submitLink(link);
              if (stage === "code") submitCode();
              if (stage === "result") close();
            }}
          >
            <DialogBody>
              <DialogTitle>{t("Import from Another Device")}</DialogTitle>
              {stage === "link" && (
                <DialogContent className="dialog-stack">
                  <Field label={t("Paste Transfer Link")} validationMessage={linkError ?? undefined}>
                    <Input
                      value={link}
                      placeholder={t("Paste the transfer link here")}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(_, data) => {
                        setLink(data.value);
                        setLinkError(null);
                      }}
                    />
                  </Field>
                  {scanning ? (
                    <div className="import-camera">
                      <video ref={video} muted playsInline />
                      <Text size={200} className="secondary">
                        {t("Point the camera at the QR code on your other device.")}
                      </Text>
                    </div>
                  ) : (
                    <div>
                      <Button icon={<CameraRegular />} onClick={startCamera}>
                        {t("Scan QR Code")}
                      </Button>
                    </div>
                  )}
                  {cameraError && (
                    <Text size={200} className="text-error">
                      {cameraError}
                    </Text>
                  )}
                </DialogContent>
              )}
              {stage === "code" && (
                <DialogContent className="dialog-stack">
                  <Field
                    label={t("Transfer Code")}
                    hint={codeError ? undefined : t("Enter the transfer code shown on the other device.")}
                    validationMessage={codeError ?? undefined}
                  >
                    <TransferCodeInput
                      inputRef={codeInput}
                      value={code}
                      onChange={(value) => {
                        setCode(value);
                        setCodeError(null);
                      }}
                    />
                  </Field>
                </DialogContent>
              )}
              {stage === "result" && saved && (
                <DialogContent className="dialog-stack">
                  <Text weight="semibold">
                    {saved.updated
                      ? t("“{0}” was updated.", saved.destination.name)
                      : t("“{0}” was added.", saved.destination.name)}
                  </Text>
                  {isTesting ? (
                    <Spinner size="tiny" labelPosition="after" label={t("Testing…")} className="import-testing" />
                  ) : (
                    <ConnectionTestResult result={testResult} error={testError} />
                  )}
                </DialogContent>
              )}
              {stage === "result" ? (
                <DialogActions>
                  <Button appearance="secondary" onClick={() => saved && onEdit(saved.destination)}>
                    {t("Edit")}
                  </Button>
                  <Button ref={doneButton} appearance="primary" type="submit">
                    {t("Done")}
                  </Button>
                </DialogActions>
              ) : (
                <DialogActions>
                  <Button appearance="secondary" onClick={close}>
                    {t("Cancel")}
                  </Button>
                  <Button
                    appearance="primary"
                    type="submit"
                    disabled={isWorking || (stage === "link" ? !link.trim() : !normalizeCode(code))}
                    icon={isWorking ? <Spinner size="tiny" /> : undefined}
                  >
                    {t("Continue")}
                  </Button>
                </DialogActions>
              )}
            </DialogBody>
          </form>
        </DialogSurface>
      </Dialog>
      <ConfirmDialog
        open={open && stage === "duplicate"}
        title={t("“{0}” is already on this device.", existingName)}
        message={
          movesUploads
            ? t(
                "The imported settings upload to a different place than “{0}” does now. Only update it if you trust where this link came from.",
                existingName,
              )
            : undefined
        }
        confirmLabel={t("Update Existing")}
        alternative={{ label: t("Add as Copy"), onSelect: () => saveDuplicate("copy") }}
        onConfirm={() => saveDuplicate("update")}
        onCancel={close}
      />
    </>
  );
}

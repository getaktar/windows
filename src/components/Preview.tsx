import { Button, Spinner, Text } from "@fluentui/react-components";
import { ImageOffRegular, OpenRegular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";

import { api } from "../lib/api";
import { previewKind } from "../lib/format";
import { useI18n } from "../lib/i18n";
import { FileIcon } from "./FileVisuals";
import { Markdown } from "./Markdown";

interface PreviewProps {
  /** Where to load the file from: the public URL, or a presigned one. */
  url: string | null;
  filename: string;
  mimeType?: string;
  /** Opened by "Open in Browser" (the public link, even when `url` is presigned). */
  browserURL: string;
  /** A local thumbnail to show, dimmed, while the full image loads. */
  placeholder?: string | null;
  onZoom?: () => void;
}

type Loaded = { state: "loading" } | { state: "failed" } | { state: "ready"; data: ArrayBuffer };

/** Downloads a file for an inline preview (PDF, text, Markdown). The
 * uploaded file isn't kept locally, so this fetches it each time. */
function useRemoteFile(url: string | null, enabled: boolean): Loaded {
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setLoaded({ state: "loading" });
    if (!url) return;
    api
      .fetchRemote(url)
      .then((data) => !cancelled && setLoaded({ state: "ready", data }))
      .catch(() => !cancelled && setLoaded({ state: "failed" }));
    return () => {
      cancelled = true;
    };
  }, [url, enabled]);
  return loaded;
}

function Unavailable({ browserURL }: { browserURL: string }) {
  const { t } = useI18n();
  return (
    <div className="preview-message">
      <ImageOffRegular fontSize={28} />
      <Text size={200}>{t("Preview unavailable")}</Text>
      <Button size="small" appearance="transparent" icon={<OpenRegular />} onClick={() => api.openUrl(browserURL)}>
        {t("Open in Browser")}
      </Button>
    </div>
  );
}

function PdfFrame({ data }: { data: ArrayBuffer }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    const objectURL = URL.createObjectURL(new Blob([data], { type: "application/pdf" }));
    setSrc(objectURL);
    return () => URL.revokeObjectURL(objectURL);
  }, [data]);
  return src ? <iframe className="preview-pdf" src={src} title="PDF" /> : null;
}

export function Preview({ url, filename, mimeType, browserURL, placeholder, onZoom }: PreviewProps) {
  const { t } = useI18n();
  const kind = previewKind(filename, mimeType);
  const file = useRemoteFile(url, kind.kind === "pdf" || kind.kind === "text");
  const [imageState, setImageState] = useState<"loading" | "ready" | "failed">("loading");

  useEffect(() => setImageState("loading"), [url]);

  switch (kind.kind) {
    case "image":
      return (
        <button type="button" className="preview preview-image checkerboard" onClick={onZoom} disabled={!onZoom} title={filename}>
          {imageState === "failed" ? (
            <Unavailable browserURL={browserURL} />
          ) : (
            <>
              {imageState === "loading" &&
                (placeholder ? <img className="preview-placeholder" src={placeholder} alt="" /> : <Spinner size="small" />)}
              {url && (
                <img
                  src={url}
                  alt=""
                  draggable={false}
                  style={imageState === "ready" ? undefined : { display: "none" }}
                  onLoad={() => setImageState("ready")}
                  onError={() => setImageState("failed")}
                />
              )}
            </>
          )}
        </button>
      );
    case "pdf":
      return (
        <div className="preview preview-tall">
          {file.state === "ready" ? (
            <PdfFrame data={file.data} />
          ) : file.state === "failed" ? (
            <Unavailable browserURL={browserURL} />
          ) : (
            <Spinner size="small" />
          )}
        </div>
      );
    case "text": {
      let text: string | null = null;
      if (file.state === "ready") {
        try {
          text = new TextDecoder("utf-8", { fatal: true }).decode(file.data);
        } catch {
          text = null;
        }
      }
      return (
        <div className="preview preview-text">
          {file.state === "loading" ? (
            <Spinner size="small" />
          ) : text === null ? (
            <Unavailable browserURL={browserURL} />
          ) : kind.markdown ? (
            <Markdown source={text} />
          ) : (
            <pre className="preview-plain">{text}</pre>
          )}
        </div>
      );
    }
    default:
      return (
        <button type="button" className="preview preview-file" onClick={() => api.openUrl(browserURL)}>
          <FileIcon filename={filename} size={44} />
          <Text size={200} className="link-text">
            {t("Open in Browser")}
          </Text>
        </button>
      );
  }
}

import { Button, Spinner, Text } from "@fluentui/react-components";
import { ImageOffRegular, OpenRegular, PlayCircleFilled } from "@fluentui/react-icons";
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
  /** A local thumbnail to show, dimmed, while the full image loads, and in
   * place of a preview for other files (a video's frame, a document's
   * first page). */
  placeholder?: string | null;
  /** The link a video or audio file plays from, asked for on Play: a
   * presigned one where possible, so private buckets work too. Without it,
   * `url`. */
  mediaURL?: () => Promise<string | null>;
  /** The file's size in bytes, when known: past 25 MB, nothing is downloaded. */
  size?: number;
  onZoom?: () => void;
}

type Loaded = { state: "loading" } | { state: "failed" } | { state: "ready"; data: ArrayBuffer };

/** The biggest file downloaded for a preview; Rust stops there too. */
const MAX_PREVIEW_BYTES = 25 * 1024 * 1024;

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

/** A video or audio file, played from its link without being downloaded
 * first. Nothing loads until Play is clicked; until then its thumbnail
 * stands in. */
function MediaPlayer(props: { video: boolean; filename: string; poster?: string | null; load: () => Promise<string | null>; browserURL: string }) {
  const { t } = useI18n();
  const [src, setSrc] = useState<string | null>(null);
  const [state, setState] = useState<"idle" | "loading" | "failed">("idle");
  const play = async () => {
    setState("loading");
    const url = await props.load().catch(() => null);
    if (url) setSrc(url);
    else setState("failed");
  };
  if (state === "failed") return <Unavailable browserURL={props.browserURL} />;
  if (src) {
    return props.video ? (
      <video className="preview-media" src={src} controls autoPlay onError={() => setState("failed")} />
    ) : (
      <audio className="preview-audio" src={src} controls autoPlay onError={() => setState("failed")} />
    );
  }
  return (
    <>
      {props.poster ? <img className="preview-poster" src={props.poster} alt="" draggable={false} /> : <FileIcon filename={props.filename} size={44} />}
      {state === "loading" ? (
        <Spinner className="preview-play" size="medium" />
      ) : (
        <button type="button" className="preview-play" aria-label={t("Play")} title={t("Play")} onClick={play}>
          <PlayCircleFilled fontSize={56} />
        </button>
      )}
    </>
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

export function Preview({ url, filename, mimeType, browserURL, placeholder, mediaURL, size, onZoom }: PreviewProps) {
  const { t } = useI18n();
  const kind = previewKind(filename, mimeType);
  const tooBig = size !== undefined && size > MAX_PREVIEW_BYTES;
  const downloaded = (kind.kind === "pdf" || kind.kind === "text") && !tooBig;
  const file = useRemoteFile(url, downloaded);
  const [imageState, setImageState] = useState<"loading" | "ready" | "failed">("loading");

  useEffect(() => setImageState("loading"), [url]);

  if (tooBig && (kind.kind === "pdf" || kind.kind === "text")) {
    return (
      <div className="preview preview-text">
        <Unavailable browserURL={browserURL} />
      </div>
    );
  }

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
    case "media":
      return (
        <div className="preview preview-player">
          <MediaPlayer
            key={url ?? filename}
            video={kind.video}
            filename={filename}
            poster={placeholder}
            load={mediaURL ?? (() => Promise.resolve(url))}
            browserURL={browserURL}
          />
        </div>
      );
    default:
      return (
        <button type="button" className="preview preview-file" onClick={() => api.openUrl(browserURL)}>
          {placeholder ? (
            <img className="preview-thumbnail" src={placeholder} alt="" draggable={false} />
          ) : (
            <FileIcon filename={filename} size={44} />
          )}
          <Text size={200} className="link-text">
            {t("Open in Browser")}
          </Text>
        </button>
      );
  }
}

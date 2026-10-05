import { Badge } from "@fluentui/react-components";
import {
  CloudRegular,
  DatabaseRegular,
  DocumentPdfRegular,
  DocumentRegular,
  DocumentTextRegular,
  FolderZipRegular,
  HardDriveRegular,
  ImageRegular,
  MusicNote2Regular,
  ServerRegular,
  StorageRegular,
  TimerRegular,
  VideoRegular,
  BoxRegular,
} from "@fluentui/react-icons";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

import { api, events, type BucketObject, type ProviderPreset, type UploadRecord } from "../lib/api";
import { expiryLabel, fileKind, formatDateTime } from "../lib/format";
import { useI18n } from "../lib/i18n";

/** A generic icon for a file, based on its extension, wherever a real
 * thumbnail isn't available. */
export function FileIcon({ filename, size = 20 }: { filename: string; size?: number }) {
  const style = { fontSize: size };
  switch (fileKind(filename)) {
    case "image":
      return <ImageRegular style={style} />;
    case "pdf":
      return <DocumentPdfRegular style={style} />;
    case "archive":
      return <FolderZipRegular style={style} />;
    case "video":
      return <VideoRegular style={style} />;
    case "audio":
      return <MusicNote2Regular style={style} />;
    case "text":
      return <DocumentTextRegular style={style} />;
    default:
      return <DocumentRegular style={style} />;
  }
}

export function ProviderIcon({ preset, size = 20 }: { preset: ProviderPreset; size?: number }) {
  const style = { fontSize: size };
  switch (preset) {
    case "cloudflareR2":
      return <CloudRegular style={style} />;
    case "amazonS3":
      return <BoxRegular style={style} />;
    case "minIO":
      return <ServerRegular style={style} />;
    case "backblazeB2":
      return <HardDriveRegular style={style} />;
    case "digitalOceanSpaces":
      return <DatabaseRegular style={style} />;
    case "customS3":
      return <StorageRegular style={style} />;
  }
}

/** History entries whose missing thumbnail was asked for in this session:
 * once each, however often their rows show. */
const requested = new Set<string>();

/** The upload's thumbnail on this PC. One it doesn't have is asked for once
 * the row shows (made from the file in the bucket, or fetched from the
 * bucket's thumbnail folder); History refreshes when it arrives. */
export function useThumbnailURL(record: UploadRecord | null) {
  const path = record?.thumbnailPath ?? null;
  const id = record?.id;
  useEffect(() => {
    if (!id || path || requested.has(id)) return;
    requested.add(id);
    api.loadRecordThumbnail(id).catch(() => {});
  }, [id, path]);
  return path ? convertFileSrc(path) : null;
}

/** Square, aspect-fit thumbnail on a faint checkerboard so transparent PNGs
 * stay legible; a file-kind icon for everything else. */
export function Thumbnail({ record, size }: { record: UploadRecord; size: number }) {
  const url = useThumbnailURL(record);
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  const radius = Math.round(size * 0.18);
  if (url && !failed) {
    return (
      <div className="thumb checkerboard" style={{ width: size, height: size, borderRadius: radius }}>
        <img src={url} alt="" draggable={false} onError={() => setFailed(true)} />
      </div>
    );
  }
  return (
    <div className="thumb thumb-icon" style={{ width: size, height: size, borderRadius: radius }}>
      <FileIcon filename={record.localFilename} size={Math.round(size * 0.45)} />
    </div>
  );
}

/** "Deletes in 5 days" on an expiring upload, with the exact time on hover. */
export function ExpiryBadge({ expiresAt }: { expiresAt: number | null }) {
  const { t, locale } = useI18n();
  if (expiresAt === null) return null;
  const expired = expiresAt <= Date.now();
  return (
    <Badge
      className="expiry-badge"
      appearance="tint"
      color={expired ? "danger" : "warning"}
      size="small"
      icon={<TimerRegular />}
      title={formatDateTime(expiresAt, locale, "full")}
    >
      {expiryLabel(expiresAt, Date.now(), t)}
    </Badge>
  );
}

/** A bucket file's thumbnail (see `api.bucketThumbnail`), or its file icon
 * while there's none, and for good when the destination's thumbnails are
 * off. */
export function ObjectThumbnail({ destinationId, object, size }: { destinationId: string; object: BucketObject; size: number }) {
  const url = useObjectThumbnail(destinationId, object);
  const radius = Math.round(size * 0.18);
  if (url) {
    return (
      <div className="thumb checkerboard" style={{ width: size, height: size, borderRadius: radius }}>
        <img src={url} alt="" draggable={false} />
      </div>
    );
  }
  return <FileIcon filename={object.key} size={Math.round(size * 0.78)} />;
}

/** Thumbnails already loaded, by destination, key, size and date, so rows
 * scrolled back into view don't ask again. */
const objectThumbnails = new Map<string, string | null>();

export function useObjectThumbnail(destinationId: string, object: BucketObject | null) {
  const cacheKey = object ? [destinationId, object.key, object.size, object.lastModified ?? 0].join("\n") : "";
  const [url, setUrl] = useState<string | null>(() => objectThumbnails.get(cacheKey) ?? null);
  useEffect(() => {
    if (!object) return;
    if (objectThumbnails.has(cacheKey)) {
      setUrl(objectThumbnails.get(cacheKey) ?? null);
      return;
    }
    let cancelled = false;
    setUrl(null);
    api
      .bucketThumbnail(destinationId, object)
      .then((loaded) => {
        // None may be offline for now: asked again when the row shows again.
        if (loaded) objectThumbnails.set(cacheKey, loaded);
        if (!cancelled) setUrl(loaded);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
    // The object is identified by `cacheKey`.
  }, [cacheKey]);
  return url;
}

// After Settings > General > Clear (in another window), or a change to a
// destination's thumbnails, what's loaded here no longer applies.
for (const event of [events.thumbnailsCleared, events.destinationsChanged]) {
  listen(event, () => {
    objectThumbnails.clear();
    requested.clear();
  }).catch(() => {});
}

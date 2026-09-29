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
import { useEffect, useState } from "react";

import { api, type ProviderPreset, type UploadRecord } from "../lib/api";
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

let thumbnailsDir: Promise<string> | null = null;

function thumbnailURL(dir: string, id: string) {
  const separator = dir.includes("\\") ? "\\" : "/";
  return convertFileSrc(`${dir}${separator}${id}.png`);
}

export function useThumbnailURL(record: UploadRecord | null) {
  const [dir, setDir] = useState<string | null>(null);
  useEffect(() => {
    thumbnailsDir ??= api.thumbnailsDir();
    thumbnailsDir.then(setDir).catch(() => {});
  }, []);
  if (!record?.hasThumbnail || !dir) return null;
  return thumbnailURL(dir, record.id);
}

/** Square, aspect-fit thumbnail on a faint checkerboard so transparent PNGs
 * stay legible; a file-kind icon for everything else. */
export function Thumbnail({ record, size }: { record: UploadRecord; size: number }) {
  const url = useThumbnailURL(record);
  const [failed, setFailed] = useState(false);
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

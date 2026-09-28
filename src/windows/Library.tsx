import { Text } from "@fluentui/react-components";
import { ArrowDownloadRegular, HistoryRegular } from "@fluentui/react-icons";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";

import { ProviderIcon } from "../components/FileVisuals";
import { api, type DestinationConfig } from "../lib/api";
import { useDestinations } from "../lib/hooks";
import { useI18n } from "../lib/i18n";
import { BucketView } from "./library/BucketView";
import { HistoryView } from "./library/HistoryView";

type Source = { kind: "history" } | { kind: "bucket"; id: string };

/** A browser is rebuilt when its destination's connection settings change,
 * but not when only the default destination changes. */
function browserKey(destination: DestinationConfig) {
  const { isDefault: _ignored, ...rest } = destination;
  return JSON.stringify(rest);
}

/** The Library window: upload history, plus a live browser for each
 * destination's bucket. */
export default function Library() {
  const { t } = useI18n();
  const [destinations] = useDestinations();
  const [source, setSource] = useState<Source>({ kind: "history" });
  // Buckets opened so far stay mounted (hidden), so switching back returns
  // to the folder you were in.
  const [visited, setVisited] = useState<string[]>([]);
  const [isDropTargeted, setIsDropTargeted] = useState(false);
  const uploadTargets = useRef(new Map<string, (paths: string[]) => void>());
  const sourceRef = useRef(source);
  sourceRef.current = source;

  const registerUpload = useCallback((id: string, upload: ((paths: string[]) => void) | null) => {
    if (upload) uploadTargets.current.set(id, upload);
    else uploadTargets.current.delete(id);
  }, []);

  useEffect(() => {
    if (source.kind === "bucket" && !destinations.some((destination) => destination.id === source.id) && destinations.length > 0) {
      setSource({ kind: "history" });
    }
    setVisited((current) => current.filter((id) => destinations.some((destination) => destination.id === id)));
  }, [destinations, source]);

  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === "enter" || payload.type === "over") {
        setIsDropTargeted(true);
      } else if (payload.type === "leave") {
        setIsDropTargeted(false);
      } else if (payload.type === "drop") {
        setIsDropTargeted(false);
        if (payload.paths.length === 0) return;
        const current = sourceRef.current;
        const bucketUpload = current.kind === "bucket" ? uploadTargets.current.get(current.id) : undefined;
        if (bucketUpload) bucketUpload(payload.paths);
        else api.uploadFiles(payload.paths);
      }
    });
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  const openBucket = (id: string) => {
    setVisited((current) => (current.includes(id) ? current : [...current, id]));
    setSource({ kind: "bucket", id });
  };

  return (
    <div className="library">
      <nav className="library-sources">
        <Text size={200} weight="semibold" className="secondary sources-heading">
          {t("Library")}
        </Text>
        <button
          type="button"
          className={`source ${source.kind === "history" ? "source-selected" : ""}`}
          onClick={() => setSource({ kind: "history" })}
        >
          <HistoryRegular fontSize={18} />
          <span className="ellipsis">{t("History")}</span>
        </button>
        {destinations.length > 0 && (
          <Text size={200} weight="semibold" className="secondary sources-heading">
            {t("Buckets")}
          </Text>
        )}
        {destinations.map((destination) => (
          <button
            type="button"
            key={destination.id}
            title={destination.bucket}
            className={`source ${source.kind === "bucket" && source.id === destination.id ? "source-selected" : ""}`}
            onClick={() => openBucket(destination.id)}
          >
            <ProviderIcon preset={destination.preset} size={18} />
            <span className="ellipsis">{destination.name}</span>
          </button>
        ))}
      </nav>

      <HistoryView active={source.kind === "history"} />
      {visited.map((id) => {
        const destination = destinations.find((candidate) => candidate.id === id);
        if (!destination) return null;
        return (
          <BucketView
            key={browserKey(destination)}
            destination={destination}
            active={source.kind === "bucket" && source.id === id}
            registerUpload={registerUpload}
          />
        );
      })}

      {isDropTargeted && (
        <div className="drop-overlay">
          <div className="drop-overlay-frame">
            <ArrowDownloadRegular fontSize={40} className="accent" />
            <Text size={500} weight="semibold">
              {t("Release to upload")}
            </Text>
          </div>
        </div>
      )}
    </div>
  );
}

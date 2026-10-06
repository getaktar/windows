import { listen, type EventCallback } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";

import {
  api,
  events,
  type DestinationConfig,
  type Job,
  type LocalApiState,
  type Settings,
  type ShortLinkDefinition,
  type UpdateStatus,
  type UploadRecord,
  type WatchOverview,
} from "./api";

/** Subscribes to a Rust event for the lifetime of the component. */
export function useTauriEvent<T>(name: string, handler: EventCallback<T>) {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;
  useEffect(() => {
    const unlisten = listen<T>(name, (event) => handlerRef.current(event));
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [name]);
}

/**
 * Loads a value from Rust and reloads it whenever one of `eventNames`
 * fires, so every window stays in sync with the app's state.
 */
export function useLive<T>(load: () => Promise<T>, eventNames: string[], initial: T): [T, () => void] {
  const [value, setValue] = useState<T>(initial);
  const loadRef = useRef(load);
  loadRef.current = load;
  const generation = useRef(0);

  const refresh = useCallback(() => {
    const current = ++generation.current;
    loadRef.current()
      .then((next) => {
        if (current === generation.current) setValue(next);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const stops = eventNames.map((name) => listen(name, refresh));
    return () => {
      stops.forEach((stop) => stop.then((unlisten) => unlisten()));
    };
  }, [refresh, eventNames.join("|")]);

  return [value, refresh];
}

export const useDestinations = () =>
  useLive<DestinationConfig[]>(api.listDestinations, [events.destinationsChanged, events.languageChanged], []);

export const useJobs = () => useLive<Job[]>(api.listJobs, [events.jobsChanged], []);

export const useHistory = () => useLive<UploadRecord[]>(api.listHistory, [events.historyChanged], []);

export const useSettings = () => useLive<Settings | null>(api.getSettings, [events.settingsChanged], null);

export const useLocalApi = () => useLive<LocalApiState | null>(api.localApiState, [events.localApiChanged], null);

export const useWatched = () =>
  useLive<WatchOverview | null>(api.watchedFolders, [events.watchedChanged, events.destinationsChanged], null);

export const useUpdateStatus = () => useLive<UpdateStatus>(api.updateStatus, [events.updateChanged], { kind: "idle" });

/** True while the user has text selected (a file name or link in a detail
 * pane), when Ctrl+C must copy that text rather than the selected item. */
export function hasTextSelection() {
  return (window.getSelection()?.toString() ?? "").length > 0;
}

/** Shows "Copied" (or similar) for a moment after an action. */
export function useFlag(duration = 1500): [boolean, () => void] {
  const [on, setOn] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  const trigger = useCallback(() => {
    setOn(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setOn(false), duration);
  }, [duration]);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  return [on, trigger];
}

/**
 * Click, Ctrl+click, and Shift+click selection over an ordered list of
 * IDs, like a File Explorer list.
 */
export function useSelection(ids: string[]) {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const anchor = useRef<string | null>(null);

  // Drop IDs that disappeared (deleted, filtered out).
  useEffect(() => {
    setSelected((current) => {
      const next = new Set([...current].filter((id) => ids.includes(id)));
      return next.size === current.size ? current : next;
    });
  }, [ids]);

  const click = useCallback(
    (id: string, event: { ctrlKey: boolean; metaKey: boolean; shiftKey: boolean }) => {
      if (event.shiftKey && anchor.current && ids.includes(anchor.current)) {
        const [from, to] = [ids.indexOf(anchor.current), ids.indexOf(id)].sort((a, b) => a - b);
        setSelected(new Set(ids.slice(from, to + 1)));
        return;
      }
      anchor.current = id;
      if (event.ctrlKey || event.metaKey) {
        setSelected((current) => {
          const next = new Set(current);
          if (next.has(id)) next.delete(id);
          else next.add(id);
          return next;
        });
      } else {
        setSelected(new Set([id]));
      }
    },
    [ids],
  );

  /** Arrow key navigation; returns the newly selected ID. */
  const move = useCallback(
    (delta: number) => {
      if (ids.length === 0) return null;
      const current = anchor.current && ids.includes(anchor.current) ? ids.indexOf(anchor.current) : -1;
      const next = ids[Math.max(0, Math.min(ids.length - 1, current + delta))];
      anchor.current = next;
      setSelected(new Set([next]));
      return next;
    },
    [ids],
  );

  const set = useCallback((next: string[]) => {
    anchor.current = next[0] ?? null;
    setSelected(new Set(next));
  }, []);

  return { selected, click, move, set };
}

/** The built-in link shorteners (the shared short-link-providers.json),
 * read once; null until they're there. */
let shortLinkProviders: ShortLinkDefinition[] | null = null;
export function useShortLinkProviders(): ShortLinkDefinition[] | null {
  const [providers, setProviders] = useState(shortLinkProviders);
  useEffect(() => {
    if (providers) return;
    let current = true;
    api
      .shortLinkProviders()
      .then((result) => {
        shortLinkProviders = result.providers;
        if (current) setProviders(result.providers);
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [providers]);
  return providers;
}

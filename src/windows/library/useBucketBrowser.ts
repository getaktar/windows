// Live, folder-by-folder view of one destination's bucket, ported from the
// Mac app's BucketBrowserModel. S3 has no real folders, so a "folder" is a
// key prefix ending in "/", listed one level at a time with a "/" delimiter
// and paged 1000 keys per request.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorMessage, events, type BucketObject, type DestinationConfig, type UploadSucceeded } from "../../lib/api";
import { folderDisplayName, nameOfKey, parentOfFolder, parentOfKey, resolvePublicUrl } from "../../lib/format";
import { useTauriEvent } from "../../lib/hooks";

export const MAX_SEARCH_RESULTS = 2000;

export type SearchScope = "bucket" | "folder";

interface SearchIndex {
  prefix: string;
  objects: BucketObject[];
  folders: Set<string>;
  next: string | null;
  complete: boolean;
}

interface SearchState {
  results: BucketObject[];
  folderResults: string[];
  searching: boolean;
  scanned: number;
  complete: boolean;
  error: string | null;
}

const emptySearch: SearchState = { results: [], folderResults: [], searching: false, scanned: 0, complete: false, error: null };

/** Case- and accent-insensitive, like Foundation's localizedStandardContains. */
function fold(text: string) {
  return text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLocaleLowerCase();
}

/** The folders between `prefix` and `key`, e.g. "a/" and "a/b/" for
 * "a/b/c.png" under the bucket root. */
function foldersContaining(key: string, prefix: string) {
  if (!key.startsWith(prefix)) return [];
  const parts = key.slice(prefix.length).split("/").slice(0, -1);
  const result: string[] = [];
  let running = prefix;
  for (const part of parts) {
    running += `${part}/`;
    result.push(running);
  }
  return result;
}

/** The folder directly under `prefix` that contains `key`, if any. */
function topLevelFolder(key: string, prefix: string) {
  if (!key.startsWith(prefix)) return null;
  const rest = key.slice(prefix.length);
  const slash = rest.indexOf("/");
  return slash < 0 ? null : prefix + rest.slice(0, slash + 1);
}

const byKey = (a: BucketObject, b: BucketObject) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0);

export function useBucketBrowser(destination: DestinationConfig) {
  const id = destination.id;
  const [prefix, setPrefix] = useState("");
  const [folders, setFolders] = useState<string[]>([]);
  const [objects, setObjects] = useState<BucketObject[]>([]);
  const [nextToken, setNextToken] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [hasLoaded, setHasLoaded] = useState(false);
  const [busyKeys, setBusyKeys] = useState<Set<string>>(new Set());
  const [actionError, setActionError] = useState<string | null>(null);
  const [searchText, setSearchText] = useState("");
  const [searchScope, setSearchScope] = useState<SearchScope>("bucket");
  const [search, setSearch] = useState<SearchState>(emptySearch);
  const [searchGeneration, setSearchGeneration] = useState(0);

  const loadGeneration = useRef(0);
  const prefixRef = useRef(prefix);
  prefixRef.current = prefix;
  const index = useRef<SearchIndex | null>(null);
  const previewURLs = useRef(new Map<string, { url: string; expires: number }>());

  const query = searchText.trim();
  const isSearchActive = query !== "";
  const searchPrefix = searchScope === "folder" ? prefix : "";

  // MARK: Listing

  const load = useCallback(
    async (forPrefix: string, token: string | null) => {
      const generation = ++loadGeneration.current;
      setIsLoading(true);
      setLoadError(null);
      try {
        const page = await api.listObjects(id, forPrefix, token, false);
        if (generation !== loadGeneration.current) return;
        setFolders((current) => (token ? [...current, ...page.folders] : page.folders));
        setObjects((current) => (token ? [...current, ...page.objects] : page.objects));
        setNextToken(page.nextContinuationToken);
        setHasLoaded(true);
      } catch (error) {
        if (generation === loadGeneration.current) setLoadError(errorMessage(error));
      } finally {
        if (generation === loadGeneration.current) setIsLoading(false);
      }
    },
    [id],
  );

  const reload = useCallback(() => {
    setFolders([]);
    setObjects([]);
    setNextToken(null);
    setHasLoaded(false);
    load(prefixRef.current, null);
  }, [load]);

  useEffect(() => {
    reload();
  }, [reload]);

  const open = useCallback(
    (folder: string) => {
      prefixRef.current = folder;
      setPrefix(folder);
      setSearchText("");
      reload();
    },
    [reload],
  );

  /** Reloads the open folder and forgets what search has listed so far. */
  const refresh = useCallback(() => {
    index.current = null;
    setSearchGeneration((generation) => generation + 1);
    reload();
  }, [reload]);

  const loadMore = useCallback(() => {
    if (nextToken && !isLoading) load(prefixRef.current, nextToken);
  }, [nextToken, isLoading, load]);

  // MARK: Search

  const applySearch = useCallback((text: string) => {
    const current = index.current;
    if (!current) return;
    const needle = fold(text);
    const results: BucketObject[] = [];
    for (const object of current.objects) {
      if (fold(nameOfKey(object.key)).includes(needle)) {
        results.push(object);
        if (results.length >= MAX_SEARCH_RESULTS) break;
      }
    }
    const folderResults = [...current.folders].filter((folder) => fold(folderDisplayName(folder)).includes(needle)).sort();
    setSearch((state) => ({
      ...state,
      results,
      folderResults,
      scanned: current.objects.length,
      complete: current.complete,
    }));
  }, []);

  useEffect(() => {
    if (!query) {
      setSearch(emptySearch);
      return;
    }
    let cancelled = false;
    // Wait for a pause in typing before listing anything.
    const timer = window.setTimeout(async () => {
      if (index.current?.prefix !== searchPrefix) {
        index.current = { prefix: searchPrefix, objects: [], folders: new Set(), next: null, complete: false };
      }
      setSearch((state) => ({ ...state, error: null }));
      applySearch(query);
      if (index.current.complete) {
        setSearch((state) => ({ ...state, searching: false }));
        return;
      }
      setSearch((state) => ({ ...state, searching: true }));
      const current = index.current;
      try {
        while (!current.complete && !cancelled && index.current === current) {
          const token = current.next;
          const page = await api.listObjects(id, searchPrefix, token, true);
          // The index was thrown away (Refresh, another scope) meanwhile.
          if (index.current !== current) return;
          // An overlapping search (the user kept typing) already added this
          // page; carry on from where it got to.
          if (current.next !== token) continue;
          current.objects.push(...page.objects);
          page.folders.forEach((folder) => current.folders.add(folder));
          for (const object of page.objects) {
            foldersContaining(object.key, searchPrefix).forEach((folder) => current.folders.add(folder));
          }
          current.next = page.nextContinuationToken;
          current.complete = page.nextContinuationToken === null;
          if (!cancelled) applySearch(query);
        }
      } catch (error) {
        if (!cancelled) setSearch((state) => ({ ...state, error: errorMessage(error) }));
      } finally {
        if (!cancelled) setSearch((state) => ({ ...state, searching: false }));
      }
    }, 250);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query, searchPrefix, searchGeneration, id, applySearch]);

  const addToIndex = useCallback(
    (object: BucketObject) => {
      const current = index.current;
      if (!current || !object.key.startsWith(current.prefix)) return;
      current.objects = current.objects.filter((existing) => existing.key !== object.key);
      current.objects.push(object);
      foldersContaining(object.key, current.prefix).forEach((folder) => current.folders.add(folder));
      if (query) applySearch(query);
    },
    [query, applySearch],
  );

  const removeFromIndex = useCallback((key: string) => {
    if (index.current) index.current.objects = index.current.objects.filter((object) => object.key !== key);
    setSearch((state) => ({
      ...state,
      results: state.results.filter((object) => object.key !== key),
      scanned: index.current?.objects.length ?? 0,
    }));
  }, []);

  /** Reloads the open folder when an upload lands in it, and adds the new
   * object to the search index. */
  useTauriEvent<UploadSucceeded>(events.uploadSucceeded, (event) => {
    const upload = event.payload;
    if (upload.destinationId !== id) return;
    addToIndex({ key: upload.objectKey, size: upload.byteSize, lastModified: Date.now() });
    if (parentOfKey(upload.objectKey) === prefixRef.current) load(prefixRef.current, null);
  });

  // MARK: Links

  const publicURL = useCallback((key: string) => resolvePublicUrl(destination.publicBaseURL, key), [destination.publicBaseURL]);

  const temporaryURL = useCallback((key: string, seconds: number) => api.bucketPresign(id, key, seconds), [id]);

  /** Previews use presigned links (cached for most of their hour), so they
   * work for private buckets and misconfigured public base URLs alike. */
  const previewURL = useCallback(
    async (key: string) => {
      const cached = previewURLs.current.get(key);
      if (cached && cached.expires > Date.now()) return cached.url;
      try {
        const url = await api.bucketPresign(id, key, 3600);
        previewURLs.current.set(key, { url, expires: Date.now() + 3000 * 1000 });
        return url;
      } catch {
        return null;
      }
    },
    [id],
  );

  // MARK: Actions

  const setBusy = (key: string, busy: boolean) =>
    setBusyKeys((current) => {
      const next = new Set(current);
      if (busy) next.add(key);
      else next.delete(key);
      return next;
    });

  /** Uploads into the open folder under each file's own name. */
  const upload = useCallback(
    async (paths: string[]) => {
      try {
        await api.bucketUpload(id, paths, prefixRef.current);
      } catch (error) {
        setActionError(errorMessage(error));
      }
    },
    [id],
  );

  const createFolder = useCallback(
    async (rawName: string) => {
      const name = rawName.trim().replace(/^\/+|\/+$/g, "");
      if (!name) return;
      try {
        const folder = await api.bucketCreateFolder(id, prefixRef.current, name);
        setFolders((current) => (current.includes(folder) ? current : [...current, folder].sort()));
        if (index.current && folder.startsWith(index.current.prefix)) {
          index.current.folders.add(folder);
          if (query) applySearch(query);
        }
      } catch (error) {
        setActionError(errorMessage(error));
      }
    },
    [id, query, applySearch],
  );

  /** Deletes one after another and stops at the first failure. Returns the
   * keys that were deleted. */
  const deleteKeys = useCallback(
    async (keys: string[]) => {
      const deleted: string[] = [];
      for (const key of keys) {
        setBusy(key, true);
        try {
          await api.bucketDelete(id, key);
          deleted.push(key);
          setObjects((current) => current.filter((object) => object.key !== key));
          removeFromIndex(key);
          previewURLs.current.delete(key);
        } catch (error) {
          setActionError(errorMessage(error));
          setBusy(key, false);
          break;
        }
        setBusy(key, false);
      }
      return deleted;
    },
    [id, removeFromIndex],
  );

  /** Renames or moves an object: the target is a full key, so changing the
   * folder part moves it. Returns the new key if it stayed in view. */
  const move = useCallback(
    async (object: BucketObject, target: string): Promise<string | null> => {
      const cleaned = target.trim().replace(/^\/+|\/+$/g, "");
      if (!cleaned || cleaned === object.key) return null;
      setBusy(object.key, true);
      try {
        const newKey = await api.bucketMove(id, object.key, cleaned);
        const moved: BucketObject = { key: newKey, size: object.size, lastModified: Date.now() };
        previewURLs.current.delete(object.key);
        removeFromIndex(object.key);
        addToIndex(moved);
        const current = prefixRef.current;
        setObjects((list) => {
          const without = list.filter((existing) => existing.key !== object.key);
          return parentOfKey(newKey) === current ? [...without, moved].sort(byKey) : without;
        });
        if (parentOfKey(newKey) !== current) {
          const folder = topLevelFolder(newKey, current);
          if (folder) setFolders((list) => (list.includes(folder) ? list : [...list, folder].sort()));
          return null;
        }
        return newKey;
      } catch (error) {
        setActionError(errorMessage(error));
        return null;
      } finally {
        setBusy(object.key, false);
      }
    },
    [id, addToIndex, removeFromIndex],
  );

  const breadcrumbs = useMemo(() => {
    const crumbs = [{ name: destination.bucket, prefix: "" }];
    let running = "";
    for (const part of prefix.split("/").filter(Boolean)) {
      running += `${part}/`;
      crumbs.push({ name: part, prefix: running });
    }
    return crumbs;
  }, [prefix, destination.bucket]);

  return {
    destination,
    prefix,
    parentPrefix: prefix ? parentOfFolder(prefix) : null,
    breadcrumbs,
    folders,
    objects,
    nextToken,
    isLoading,
    loadError,
    hasLoaded,
    busyKeys,
    actionError,
    setActionError,
    searchText,
    setSearchText,
    searchScope,
    setSearchScope,
    isSearchActive,
    search,
    visibleFolders: isSearchActive ? search.folderResults : folders,
    visibleObjects: isSearchActive ? search.results : objects,
    open,
    refresh,
    reload,
    loadMore,
    publicURL,
    temporaryURL,
    previewURL,
    upload,
    createFolder,
    deleteKeys,
    move,
  };
}

export type BucketBrowser = ReturnType<typeof useBucketBrowser>;

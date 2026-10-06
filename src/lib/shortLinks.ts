// What the destination form's Short Links section edits, and how it becomes
// `DestinationConfig.shortLinks`: the Mac app's ShortLinkFormState. Rust
// checks the settings again when they're saved.

import type {
  DestinationConfig,
  ShareXImport,
  ShortLinkBodyType,
  ShortLinkDefinition,
  ShortLinkSettings,
  UploadRecord,
} from "./api";
import type { Translate } from "./i18n";

export const customProviderId = "custom";
/** The methods a custom create request can use. */
export const customMethods = ["POST", "GET", "PUT", "PATCH"];
/** The methods a custom delete request can use. */
export const deleteMethods = ["DELETE", "GET", "POST"];

/** The token is only what's typed here; empty keeps the saved one (for the
 * same provider). */
export interface ShortLinkFormState {
  /** Null is Off. */
  providerId: string | null;
  endpoint: string;
  domain: string;
  token: string;
  onlyLongerThan: number;
  shortenTemporaryLinks: boolean;
  allowInsecureHTTP: boolean;
  // Custom HTTP.
  customMethod: string;
  customPath: string;
  customHeaders: string;
  customQuery: string;
  customBodyType: ShortLinkBodyType | null;
  customBody: string;
  customShortUrlPath: string;
  customIdPath: string;
  customDeletePath: string;
  customDeleteMethod: string;
}

/** What a new custom definition starts as: a JSON POST with the link and a
 * bearer token. */
export function customTemplate(t: Translate): ShortLinkDefinition {
  return {
    id: customProviderId,
    name: t("Custom HTTP"),
    kind: "custom",
    needsDomain: false,
    create: {
      method: "POST",
      path: "/api/shorten",
      headers: { Authorization: "Bearer {token}" },
      body: { url: "{url}" },
      bodyType: "json",
      shortUrlPath: "shortUrl",
    },
    capabilities: customCapabilities(false),
  };
}

function customCapabilities(canDelete: boolean): ShortLinkDefinition["capabilities"] {
  return {
    delete: canDelete,
    updateDestination: false,
    expiration: false,
    customCode: false,
    customDomain: false,
    stats: { clicks: false, lastClick: false },
  };
}

const sortedLines = (record: Record<string, string> | undefined, separator: string) =>
  Object.keys(record ?? {})
    .sort()
    .map((key) => `${key}${separator}${record?.[key] ?? ""}`)
    .join("\n");

function withCustom(state: ShortLinkFormState, definition: ShortLinkDefinition): ShortLinkFormState {
  const create = definition.create;
  let deletePath = definition.delete?.path ?? "";
  const deleteQuery = definition.delete?.query ?? {};
  if (Object.keys(deleteQuery).length > 0) {
    deletePath += `?${Object.keys(deleteQuery)
      .sort()
      .map((key) => `${key}=${deleteQuery[key]}`)
      .join("&")}`;
  }
  return {
    ...state,
    customMethod: create.method.toUpperCase(),
    customPath: create.path,
    customHeaders: sortedLines(create.headers, ": "),
    customQuery: sortedLines(create.query, "="),
    customBodyType: create.bodyType ?? null,
    customBody: create.body !== undefined && create.body !== null ? JSON.stringify(create.body) : "",
    customShortUrlPath: create.shortUrlPath ?? "",
    customIdPath: create.idPath ?? "",
    customDeletePath: deletePath,
    customDeleteMethod: definition.delete?.method.toUpperCase() ?? "DELETE",
  };
}

export function shortLinkFormState(settings: ShortLinkSettings | null | undefined, t: Translate): ShortLinkFormState {
  const empty: ShortLinkFormState = {
    providerId: null,
    endpoint: "",
    domain: "",
    token: "",
    onlyLongerThan: 0,
    shortenTemporaryLinks: false,
    allowInsecureHTTP: false,
    customMethod: "POST",
    customPath: "",
    customHeaders: "",
    customQuery: "",
    customBodyType: "json",
    customBody: "",
    customShortUrlPath: "",
    customIdPath: "",
    customDeletePath: "",
    customDeleteMethod: "DELETE",
  };
  if (!settings) return withCustom(empty, customTemplate(t));
  return withCustom(
    {
      ...empty,
      providerId: settings.providerId,
      endpoint: settings.endpoint ?? "",
      domain: settings.domain ?? "",
      onlyLongerThan: settings.onlyLongerThan,
      shortenTemporaryLinks: settings.shortenTemporaryLinks,
      allowInsecureHTTP: settings.allowInsecureHTTP,
    },
    settings.custom ?? customTemplate(t),
  );
}

/** A ShareX configuration the user agreed to: the custom definition and its
 * token, ready to save. http:// was already refused unless allowed. */
export function applyShareXImport(state: ShortLinkFormState, imported: ShareXImport): ShortLinkFormState {
  return { ...withCustom(state, imported.definition), providerId: customProviderId, endpoint: "", token: imported.token ?? "" };
}

export const isCustom = (state: ShortLinkFormState) => state.providerId === customProviderId;

class FormProblem extends Error {}

/** The custom definition from the fields; throws what's wrong with them. */
export function customDefinition(state: ShortLinkFormState, t: Translate): ShortLinkDefinition {
  const path = state.customPath.trim();
  if (!path) throw new FormProblem(t("Enter the request’s path or URL."));
  const shortUrlPath = state.customShortUrlPath.trim();
  if (!shortUrlPath) throw new FormProblem(t("Enter where the short link is in the answer, such as shortUrl or data.link."));
  const headers: Record<string, string> = {};
  for (const line of state.customHeaders.split(/\r?\n/).filter((line) => line.trim())) {
    const colon = line.indexOf(":");
    if (colon < 0 || !line.slice(0, colon).trim()) throw new FormProblem(t("Put each header on its own line, as Name: value."));
    headers[line.slice(0, colon).trim()] = line.slice(colon + 1).trim();
  }
  const query: Record<string, string> = {};
  for (const line of state.customQuery.split(/\r?\n/).filter((line) => line.trim())) {
    const equals = line.indexOf("=");
    const name = (equals < 0 ? line : line.slice(0, equals)).trim();
    if (!name) throw new FormProblem(t("Put each query parameter on its own line, as name=value."));
    query[name] = equals < 0 ? "" : line.slice(equals + 1).trim();
  }
  let body: unknown;
  if (state.customBodyType) {
    try {
      body = JSON.parse(state.customBody.trim());
    } catch {
      body = null;
    }
    if (typeof body !== "object" || body === null || Array.isArray(body)) {
      throw new FormProblem(t('The body has to be a JSON object, such as {"url": "{url}"}.'));
    }
  }
  const deletePath = state.customDeletePath.trim();
  const idPath = state.customIdPath.trim();
  const hasHeaders = Object.keys(headers).length > 0;
  return {
    id: customProviderId,
    name: t("Custom HTTP"),
    kind: "custom",
    needsDomain: false,
    create: {
      method: state.customMethod,
      path,
      ...(Object.keys(query).length > 0 ? { query } : {}),
      ...(hasHeaders ? { headers } : {}),
      ...(state.customBodyType ? { body, bodyType: state.customBodyType } : {}),
      shortUrlPath,
      ...(idPath ? { idPath } : {}),
    },
    ...(deletePath
      ? {
          delete: {
            method: state.customDeleteMethod,
            path: deletePath,
            // The headers (and the token in them) go along only to where
            // the create request goes.
            ...(hasHeaders && sameHost(deletePath, path) ? { headers } : {}),
          },
        }
      : {}),
    capabilities: customCapabilities(deletePath !== "" && idPath !== ""),
  };
}

/** Whether the delete request is sent where the create request is: a
 * relative path, or a URL on the same host. */
export function sameHost(deletePath: string, createPath: string) {
  const host = (path: string) => {
    const lowered = path.toLowerCase();
    if (!lowered.startsWith("http://") && !lowered.startsWith("https://")) return null;
    const rest = path.slice(path.indexOf("/") + 2);
    return rest.split(/[/?#]/)[0].toLowerCase();
  };
  const deleteHost = host(deletePath);
  return deleteHost === null || deleteHost === host(createPath);
}

/** The definition picked, or the custom one as edited (null while it can't
 * be read, or before the built-in ones are loaded). */
export function formDefinition(state: ShortLinkFormState, providers: ShortLinkDefinition[] | null, t: Translate) {
  if (!state.providerId) return null;
  if (isCustom(state)) {
    try {
      return customDefinition(state, t);
    } catch {
      return null;
    }
  }
  return providers?.find((provider) => provider.id === state.providerId) ?? null;
}

export const usesEndpoint = (definition: ShortLinkDefinition) => !definition.baseUrl;
export const canExpire = (definition: ShortLinkDefinition | null) => !!definition && definition.capabilities.expiration !== false;
export const hasStats = (definition: ShortLinkDefinition) => definition.capabilities.stats.clicks || definition.capabilities.stats.lastClick;

/** The settings to save; null when Off. */
export function formSettings(state: ShortLinkFormState, providers: ShortLinkDefinition[] | null, t: Translate): ShortLinkSettings | null {
  const definition = formDefinition(state, providers, t);
  if (!state.providerId || !definition) return null;
  const endpoint = state.endpoint.trim();
  const domain = state.domain.trim();
  return {
    providerId: state.providerId,
    ...(usesEndpoint(definition) && endpoint ? { endpoint } : {}),
    ...(domain ? { domain } : {}),
    ...(isCustom(state) ? { custom: definition } : {}),
    onlyLongerThan: Math.max(0, Math.floor(state.onlyLongerThan) || 0),
    shortenTemporaryLinks: state.shortenTemporaryLinks && canExpire(definition),
    allowInsecureHTTP: state.allowInsecureHTTP,
  };
}

/** Whether any address the requests go to is http://. */
export function usesHTTP(state: ShortLinkFormState) {
  return (
    state.endpoint.trim().toLowerCase().startsWith("http://") ||
    (isCustom(state) && state.customPath.trim().toLowerCase().startsWith("http://"))
  );
}

/** What keeps these settings from being saved, given whether a token for
 * this provider is already saved. */
export function formProblem(
  state: ShortLinkFormState,
  providers: ShortLinkDefinition[] | null,
  hasSavedToken: boolean,
  t: Translate,
): string | null {
  if (!state.providerId) return null;
  if (isCustom(state)) {
    try {
      customDefinition(state, t);
    } catch (error) {
      return error instanceof Error ? error.message : String(error);
    }
  }
  const definition = formDefinition(state, providers, t);
  if (!definition) return null;
  const customIsAbsolute = isCustom(state) && state.customPath.toLowerCase().startsWith("http");
  if (usesEndpoint(definition) && !customIsAbsolute) {
    const endpoint = state.endpoint.trim();
    if (!endpoint) return t("Enter the address of your link shortener.");
    const lowered = endpoint.toLowerCase();
    if (!lowered.startsWith("https://") && !lowered.startsWith("http://") && endpoint.includes("://")) {
      return t("The link shortener’s address isn’t valid.");
    }
    const base = lowered.startsWith("https://") || lowered.startsWith("http://") ? endpoint : `https://${endpoint}`;
    try {
      if (!new URL(base).hostname) return t("The link shortener’s address isn’t valid.");
    } catch {
      return t("The link shortener’s address isn’t valid.");
    }
  }
  if (usesHTTP(state) && !state.allowInsecureHTTP) {
    return t("The link shortener’s address uses http://. Turn on “Allow insecure HTTP” to use it anyway.");
  }
  if (definition.needsDomain && !state.domain.trim()) return t("Enter the short domain.");
  if (definition.auth && !state.token.trim() && !hasSavedToken) return t("Enter the API key.");
  return null;
}

/** Whether History offers Create Short Link for an upload: its destination
 * has a shortener, and no active link was made with it (none yet, or the
 * provider was switched since). */
export function canCreateShortLink(record: UploadRecord, destinations: DestinationConfig[]) {
  const settings = destinations.find((destination) => destination.id === record.destinationId)?.shortLinks;
  return !!settings && (!record.shortUrl || record.shortProvider !== settings.providerId);
}

/** "2 hours ago", for the last click. */
export function relativeTime(date: number, locale: string) {
  const seconds = Math.round((date - Date.now()) / 1000);
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31_536_000],
    ["month", 2_592_000],
    ["week", 604_800],
    ["day", 86_400],
    ["hour", 3_600],
    ["minute", 60],
  ];
  const format = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return format.format(Math.round(seconds / size), unit);
  }
  return format.format(seconds, "second");
}

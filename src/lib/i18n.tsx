// UI strings. Keys are the English source text shared with the Mac app's
// String Catalog (see scripts/extract_locales.py), with {0}, {1}...
// placeholders. The Rust side reads the same tables.

import { listen } from "@tauri-apps/api/event";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

import { api, events, type AppInfo } from "./api";
import de from "../locales/de.json";
import en from "../locales/en.json";
import es from "../locales/es.json";
import fr from "../locales/fr.json";
import ja from "../locales/ja.json";
import ptBR from "../locales/pt-BR.json";
import tr from "../locales/tr.json";
import zhHans from "../locales/zh-Hans.json";

const tables: Record<string, Record<string, string>> = {
  en,
  tr,
  de,
  fr,
  es,
  "pt-BR": ptBR,
  ja,
  "zh-Hans": zhHans,
};

export type Translate = (key: string, ...args: (string | number)[]) => string;

interface I18n {
  t: Translate;
  /** The language in effect, e.g. "tr" or "pt-BR". */
  language: string;
  /** For Intl formatters. */
  locale: string;
  info: AppInfo | null;
  reload: () => void;
}

function translator(language: string): Translate {
  const table = tables[language] ?? tables.en;
  return (key, ...args) => {
    let text = table[key] ?? key;
    args.forEach((arg, index) => {
      text = text.split(`{${index}}`).join(String(arg));
    });
    return text;
  };
}

const I18nContext = createContext<I18n>({
  t: translator("en"),
  language: "en",
  locale: "en",
  info: null,
  reload: () => {},
});

export function I18nProvider({ children }: { children: ReactNode }) {
  const [info, setInfo] = useState<AppInfo | null>(null);

  const reload = useCallback(() => {
    api.appInfo().then(setInfo).catch(() => {});
  }, []);

  useEffect(() => {
    reload();
    const unlisten = listen(events.languageChanged, reload);
    return () => {
      unlisten.then((stop) => stop());
    };
  }, [reload]);

  const value = useMemo<I18n>(() => {
    const language = info?.language ?? "en";
    return { t: translator(language), language, locale: language, info, reload };
  }, [info, reload]);

  useEffect(() => {
    document.documentElement.lang = value.locale;
  }, [value.locale]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n() {
  return useContext(I18nContext);
}

/** getaktar.com in the app's language (the site uses lowercase prefixes). */
export function websiteURL(language: string) {
  const prefixes: Record<string, string> = {
    tr: "tr",
    de: "de",
    fr: "fr",
    es: "es",
    "pt-BR": "pt-br",
    ja: "ja",
    "zh-Hans": "zh",
  };
  const prefix = prefixes[language];
  return prefix ? `https://getaktar.com/${prefix}/` : "https://getaktar.com/";
}

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
import zhHant from "../locales/zh-Hant.json";
import ko from "../locales/ko.json";
import it from "../locales/it.json";
import nl from "../locales/nl.json";
import pl from "../locales/pl.json";
import ru from "../locales/ru.json";
import uk from "../locales/uk.json";
import id from "../locales/id.json";
import vi from "../locales/vi.json";

const tables: Record<string, Record<string, string>> = {
  en,
  tr,
  de,
  fr,
  es,
  "pt-BR": ptBR,
  ja,
  "zh-Hans": zhHans,
  "zh-Hant": zhHant,
  ko,
  it,
  nl,
  pl,
  ru,
  uk,
  id,
  vi,
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
    return { t: translator(language), language, locale: formattingLocale(language, info?.regionLocale ?? null), info, reload };
  }, [info, reload]);

  useEffect(() => {
    document.documentElement.lang = value.language;
  }, [value.language]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

/** Dates and numbers follow Windows' regional format (English UI on an
 * en-GB system: 24-hour, day first) as long as it's the same language as
 * the UI, so month names never come out in a second language. */
function formattingLocale(language: string, regionLocale: string | null) {
  if (!regionLocale) return language;
  const base = (code: string) => code.split("-")[0].toLowerCase();
  if (base(regionLocale) !== base(language)) return language;
  try {
    return Intl.getCanonicalLocales(regionLocale)[0] ?? language;
  } catch {
    return language;
  }
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
    "zh-Hant": "zh-hant",
    ko: "ko",
    it: "it",
    nl: "nl",
    pl: "pl",
    ru: "ru",
    uk: "uk",
    id: "id",
    vi: "vi",
  };
  const prefix = prefixes[language];
  return prefix ? `https://getaktar.com/${prefix}/` : "https://getaktar.com/";
}

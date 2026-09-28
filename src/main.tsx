import { FluentProvider } from "@fluentui/react-components";
import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { I18nProvider } from "./lib/i18n";
import { darkTheme, lightTheme } from "./lib/theme";
import Library from "./windows/Library";
import Onboarding from "./windows/Onboarding";
import Panel from "./windows/Panel";
import SettingsWindow from "./windows/Settings";
import Update from "./windows/Update";
import "./styles.css";

// Every window loads the same page; the route is the window's job.
const route = window.location.hash.replace(/^#\/?/, "") || "panel";

// `pnpm dev:mock` runs the UI in a browser against a fake backend.
if (import.meta.env.MODE === "mock") {
  const { installMockBackend } = await import("./dev/mockBackend");
  installMockBackend(route);
}

function usePrefersDark() {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  const [dark, setDark] = useState(query.matches);
  useEffect(() => {
    const onChange = (event: MediaQueryListEvent) => setDark(event.matches);
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, [query]);
  return dark;
}

function Root() {
  const dark = usePrefersDark();
  const screen = (() => {
    switch (route) {
      case "library":
        return <Library />;
      case "settings":
        return <SettingsWindow />;
      case "onboarding":
        return <Onboarding />;
      case "update":
        return <Update />;
      default:
        return <Panel />;
    }
  })();
  return (
    // Layout classes go on an inner element: FluentProvider copies its own
    // className onto every portal (menus, dialogs), which would make each
    // popup a full-window sheet.
    <FluentProvider theme={dark ? darkTheme : lightTheme}>
      <div className={`app app-${route}`}>
        <I18nProvider>{screen}</I18nProvider>
      </div>
    </FluentProvider>
  );
}

// These are web pages inside an app: no browser context menu (except the
// cut/copy/paste one in text fields), and no reload, print, or find.
document.addEventListener("contextmenu", (event) => {
  const target = event.target as HTMLElement | null;
  const editable = target?.closest("input, textarea, [contenteditable='true']");
  const hasSelection = (window.getSelection()?.toString() ?? "").length > 0;
  if (!editable && !hasSelection) event.preventDefault();
});
document.addEventListener("keydown", (event) => {
  const key = event.key.toLowerCase();
  const blocked =
    key === "f5" ||
    key === "f7" ||
    // Ctrl+F too: WebView2's own find bar; the Library's search handles it.
    (event.ctrlKey && (key === "r" || key === "p" || key === "g" || key === "u" || key === "j" || key === "f")) ||
    (event.ctrlKey && event.shiftKey && key === "i" && !import.meta.env.DEV);
  if (blocked) event.preventDefault();
});
// Files dropped outside a drop zone must not navigate the webview away.
window.addEventListener("dragover", (event) => event.preventDefault());
window.addEventListener("drop", (event) => event.preventDefault());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Root />
  </StrictMode>,
);

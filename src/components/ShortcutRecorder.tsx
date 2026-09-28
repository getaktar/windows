import { Button, Text } from "@fluentui/react-components";
import { DismissRegular, KeyboardRegular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";

import { api } from "../lib/api";
import { useI18n } from "../lib/i18n";

/** What each physical key types on the current keyboard layout, e.g.
 * "KeyQ" -> "a" on AZERTY. Chromium's Keyboard API; null where missing. */
type LayoutMap = { get(code: string): string | undefined };

function useLayoutMap() {
  const [layout, setLayout] = useState<LayoutMap | null>(null);
  useEffect(() => {
    const keyboard = (navigator as Navigator & { keyboard?: { getLayoutMap(): Promise<LayoutMap> } }).keyboard;
    keyboard
      ?.getLayoutMap()
      .then(setLayout)
      .catch(() => {});
  }, []);
  return layout;
}

/** "Ctrl+Shift+Alt+KeyU" -> "Ctrl + Shift + Alt + U". Letter and digit keys
 * are labeled by what they type on the current layout, since the shortcut
 * is stored by physical key. */
export function displayShortcut(accelerator: string, layout?: LayoutMap | null) {
  return accelerator
    .split("+")
    .map((part) => {
      if (part === "Super") return "Win";
      const typed = /^(Key[A-Z]|Digit\d|Minus|Equal|Bracket(Left|Right)|Backslash|Semicolon|Quote|Backquote|Comma|Period|Slash|IntlBackslash)$/.test(part)
        ? layout?.get(part)
        : undefined;
      if (typed && typed.trim()) return typed.toUpperCase();
      if (/^Key[A-Z]$/.test(part)) return part.slice(3);
      if (/^Digit\d$/.test(part)) return part.slice(5);
      if (/^Numpad\d$/.test(part)) return `Num ${part.slice(6)}`;
      if (part.startsWith("Arrow")) return part.slice(5);
      return part;
    })
    .join(" + ");
}

const modifierCodes = new Set([
  "ControlLeft", "ControlRight", "ShiftLeft", "ShiftRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "OSLeft", "OSRight",
]);

/** Builds an accelerator from the physical key, so it keeps working if the
 * keyboard layout changes. */
function acceleratorFor(event: React.KeyboardEvent): string | null {
  if (modifierCodes.has(event.code) || !event.code) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.shiftKey) parts.push("Shift");
  if (event.altKey) parts.push("Alt");
  if (event.metaKey) parts.push("Super");
  parts.push(event.code);
  return parts.join("+");
}

export function ShortcutRecorder(props: {
  value: string | null;
  error: string | null;
  onChange: (accelerator: string | null) => void;
}) {
  const { t } = useI18n();
  const layout = useLayoutMap();
  const [recording, setRecording] = useState(false);

  // The registered shortcut is taken by Windows before the page sees it,
  // so it's switched off while recording; otherwise pressing it again
  // would upload the clipboard instead of being recorded.
  const startRecording = () => {
    if (recording) return;
    setRecording(true);
    api.setShortcutPaused(true).catch(() => {});
  };
  const stopRecording = (accelerator?: string | null) => {
    if (!recording) return;
    setRecording(false);
    if (accelerator === undefined) {
      api.setShortcutPaused(false).catch(() => {});
    } else {
      // Registering the new one (or none) ends the pause on its own.
      props.onChange(accelerator);
    }
  };

  return (
    <div className="shortcut-recorder">
      <Button
        appearance={recording ? "primary" : "secondary"}
        icon={<KeyboardRegular />}
        onClick={startRecording}
        onBlur={() => stopRecording()}
        onKeyDown={(event) => {
          if (!recording) return;
          event.preventDefault();
          event.stopPropagation();
          if (event.key === "Escape") {
            stopRecording();
            return;
          }
          if ((event.key === "Backspace" || event.key === "Delete") && !event.ctrlKey && !event.altKey) {
            stopRecording(null);
            return;
          }
          const accelerator = acceleratorFor(event);
          if (accelerator) stopRecording(accelerator);
        }}
      >
        {recording ? t("Press a shortcut…") : props.value ? displayShortcut(props.value, layout) : t("Record Shortcut")}
      </Button>
      {props.value && !recording && (
        <Button
          appearance="subtle"
          icon={<DismissRegular />}
          aria-label={t("Clear")}
          title={t("Clear")}
          onClick={() => props.onChange(null)}
        />
      )}
      {props.error && (
        <Text size={200} className="text-error shortcut-error">
          {props.error}
        </Text>
      )}
    </div>
  );
}

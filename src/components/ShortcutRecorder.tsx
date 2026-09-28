import { Button, Text } from "@fluentui/react-components";
import { DismissRegular, KeyboardRegular } from "@fluentui/react-icons";
import { useState } from "react";

import { useI18n } from "../lib/i18n";

/** "Ctrl+Shift+Alt+KeyU" -> "Ctrl + Shift + Alt + U" */
export function displayShortcut(accelerator: string) {
  return accelerator
    .split("+")
    .map((part) => {
      if (part === "Super") return "Win";
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

/** Builds an accelerator from the physical key, so it doesn't depend on
 * the keyboard layout. */
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
  const [recording, setRecording] = useState(false);

  return (
    <div className="shortcut-recorder">
      <Button
        appearance={recording ? "primary" : "secondary"}
        icon={<KeyboardRegular />}
        onClick={() => setRecording(true)}
        onBlur={() => setRecording(false)}
        onKeyDown={(event) => {
          if (!recording) return;
          event.preventDefault();
          event.stopPropagation();
          if (event.key === "Escape") {
            setRecording(false);
            return;
          }
          if ((event.key === "Backspace" || event.key === "Delete") && !event.ctrlKey && !event.altKey) {
            setRecording(false);
            props.onChange(null);
            return;
          }
          const accelerator = acceleratorFor(event);
          if (accelerator) {
            setRecording(false);
            props.onChange(accelerator);
          }
        }}
      >
        {recording ? t("Press a shortcut…") : props.value ? displayShortcut(props.value) : t("Record Shortcut")}
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

import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Menu,
  MenuDivider,
  MenuItem,
  MenuList,
  MenuPopover,
  MenuTrigger,
  type PositioningVirtualElement,
} from "@fluentui/react-components";
import { useEffect, useRef, useState, type ReactNode } from "react";

import { useI18n } from "../lib/i18n";

export function ConfirmDialog(props: {
  open: boolean;
  title: string;
  /** Left out when the title says it all. */
  message?: string;
  confirmLabel: string;
  destructive?: boolean;
  /** A second way to confirm, next to the main one. */
  alternative?: { label: string; onSelect: () => void };
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  return (
    <Dialog open={props.open} onOpenChange={(_, data) => !data.open && props.onCancel()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{props.title}</DialogTitle>
          {props.message && <DialogContent>{props.message}</DialogContent>}
          <DialogActions>
            <Button appearance="secondary" onClick={props.onCancel}>
              {t("Cancel")}
            </Button>
            {props.alternative && (
              <Button appearance="secondary" onClick={props.alternative.onSelect}>
                {props.alternative.label}
              </Button>
            )}
            <Button
              appearance="primary"
              className={props.destructive ? "destructive" : undefined}
              onClick={props.onConfirm}
            >
              {props.confirmLabel}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

export function AlertDialog(props: { open: boolean; title: string; message: string; onClose: () => void }) {
  const { t } = useI18n();
  return (
    <Dialog open={props.open} onOpenChange={(_, data) => !data.open && props.onClose()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{props.title}</DialogTitle>
          <DialogContent className="selectable">{props.message}</DialogContent>
          <DialogActions>
            <Button appearance="primary" onClick={props.onClose}>
              {t("OK")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

/** A dialog with one text field, for "New Folder" and "Rename or Move". */
export function PromptDialog(props: {
  open: boolean;
  title: string;
  message?: string;
  label: string;
  initialValue: string;
  confirmLabel: string;
  onConfirm: (value: string) => void;
  onCancel: () => void;
}) {
  const { t } = useI18n();
  const [value, setValue] = useState(props.initialValue);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!props.open) return;
    setValue(props.initialValue);
    // Select the file name without its extension, like File Explorer.
    window.setTimeout(() => {
      const field = input.current;
      if (!field) return;
      field.focus();
      const slash = props.initialValue.lastIndexOf("/") + 1;
      const dot = props.initialValue.lastIndexOf(".");
      field.setSelectionRange(slash, dot > slash ? dot : props.initialValue.length);
    }, 0);
  }, [props.open, props.initialValue]);

  return (
    <Dialog open={props.open} onOpenChange={(_, data) => !data.open && props.onCancel()}>
      <DialogSurface>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            props.onConfirm(value);
          }}
        >
          <DialogBody>
            <DialogTitle>{props.title}</DialogTitle>
            <DialogContent className="dialog-stack">
              {props.message && <span>{props.message}</span>}
              <Field label={props.label}>
                <Input ref={input} value={value} onChange={(_, data) => setValue(data.value)} />
              </Field>
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={props.onCancel}>
                {t("Cancel")}
              </Button>
              <Button appearance="primary" type="submit" disabled={!value.trim()}>
                {props.confirmLabel}
              </Button>
            </DialogActions>
          </DialogBody>
        </form>
      </DialogSurface>
    </Dialog>
  );
}

export type MenuEntry =
  | { label: string; onClick: () => void; icon?: ReactNode; disabled?: boolean; destructive?: boolean }
  | { submenu: string; items: MenuEntry[]; icon?: ReactNode }
  | "divider";

export function MenuEntries({ items }: { items: MenuEntry[] }) {
  return (
    <>
      {items.map((item, index) => {
        if (item === "divider") return <MenuDivider key={index} />;
        if ("submenu" in item) {
          return (
            <Menu key={index}>
              <MenuTrigger disableButtonEnhancement>
                <MenuItem icon={item.icon as never}>{item.submenu}</MenuItem>
              </MenuTrigger>
              <MenuPopover>
                <MenuList>
                  <MenuEntries items={item.items} />
                </MenuList>
              </MenuPopover>
            </Menu>
          );
        }
        return (
          <MenuItem
            key={index}
            icon={item.icon as never}
            disabled={item.disabled}
            className={item.destructive ? "menu-destructive" : undefined}
            onClick={item.onClick}
          >
            {item.label}
          </MenuItem>
        );
      })}
    </>
  );
}

export interface ContextMenuState {
  x: number;
  y: number;
  items: MenuEntry[];
}

/** One right-click menu per list, opened at the pointer. */
export function ContextMenu({ state, onClose }: { state: ContextMenuState | null; onClose: () => void }) {
  const target: PositioningVirtualElement | undefined = state
    ? {
        getBoundingClientRect: () =>
          ({ x: state.x, y: state.y, left: state.x, top: state.y, right: state.x, bottom: state.y, width: 0, height: 0 }) as DOMRect,
      }
    : undefined;
  return (
    <Menu
      open={state !== null}
      onOpenChange={(_, data) => !data.open && onClose()}
      positioning={{ target, position: "below", align: "start" }}
    >
      <MenuPopover>
        <MenuList>{state && <MenuEntries items={state.items} />}</MenuList>
      </MenuPopover>
    </Menu>
  );
}

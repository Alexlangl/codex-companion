import * as Dialog from "@radix-ui/react-dialog";
import { useRef, useState, type ReactNode } from "react";
import { userFacingError } from "../lib/errors";

/** Avoid host-dependent window.confirm behavior in desktop WebViews. */
export function ConfirmAction({
  title,
  description,
  children,
  disabled,
  onConfirm,
  variant = "ghost",
}: {
  title: string;
  description: string;
  children: ReactNode;
  disabled?: boolean;
  onConfirm: () => Promise<void>;
  variant?: "ghost" | "danger";
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const cancelRef = useRef<HTMLButtonElement>(null);
  async function confirm() {
    setPending(true);
    setError("");
    try {
      await onConfirm();
      setOpen(false);
    } catch (cause) {
      setError(userFacingError(cause));
    } finally {
      setPending(false);
    }
  }
  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!pending) {
          setError("");
          setOpen(next);
        }
      }}
    >
      <Dialog.Trigger
        className={`button button-${variant}`}
        disabled={disabled}
      >
        {children}
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content confirm-dialog"
          role="alertdialog"
          aria-busy={pending}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            cancelRef.current?.focus();
          }}
          onPointerDownOutside={(event) => event.preventDefault()}
        >
          <Dialog.Title className="dialog-title">{title}</Dialog.Title>
          <Dialog.Description className="dialog-description">
            {description}
          </Dialog.Description>
          {error ? (
            <p role="alert" className="error-banner">
              {error}
            </p>
          ) : null}
          <div className="actions">
            <Dialog.Close
              ref={cancelRef}
              className="button button-secondary"
              disabled={pending}
            >
              取消
            </Dialog.Close>
            <button
              className="button button-danger"
              disabled={pending}
              onClick={() => void confirm()}
              type="button"
            >
              {pending ? "清理中…" : "确认清空"}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

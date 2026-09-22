import * as Dialog from "@radix-ui/react-dialog";
import { Settings2, X } from "lucide-react";
import { useRef } from "react";
import type { ReactNode } from "react";

export function SettingsDialog({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  const headingRef = useRef<HTMLHeadingElement>(null);
  return (
    <Dialog.Root>
      <Dialog.Trigger className="button button-secondary">
        <Settings2 aria-hidden="true" size={15} /> {title}
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content
          className="dialog-content settings-dialog"
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            headingRef.current?.focus();
          }}
        >
          <div className="dialog-header">
            <div>
              <Dialog.Title
                className="dialog-title"
                ref={headingRef}
                tabIndex={-1}
              >
                {title}
              </Dialog.Title>
              <Dialog.Description className="dialog-description">
                {description}
              </Dialog.Description>
            </div>
            <Dialog.Close className="icon-button" aria-label={`关闭${title}`}>
              <X aria-hidden="true" size={16} />
            </Dialog.Close>
          </div>
          <div className="settings-dialog-body">{children}</div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

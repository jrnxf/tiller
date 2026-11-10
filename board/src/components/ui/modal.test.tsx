import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef, useState } from "react";
import { afterEach, expect, test, vi } from "vitest";

import { Modal } from "./modal";

afterEach(() => {
  cleanup();
});

// A page with a button that opens a dialog of three controls, the middle one
// standing for a live terminal that Tab never wraps to.
function Page({
  onClose = () => {},
  dismissable = true,
}: {
  onClose?: () => void;
  dismissable?: boolean;
}) {
  const [isOpen, setOpen] = useState(false);
  const first = useRef<HTMLButtonElement>(null);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Open
      </button>
      <div className="toast" data-modal-exempt="" aria-live="polite" />
      {isOpen ? (
        <Modal
          className="dialog"
          label="Test dialog"
          dismissable={dismissable}
          onClose={() => {
            onClose();
            setOpen(false);
          }}
          initialFocus={first}
        >
          <button ref={first} type="button">
            First
          </button>
          <div data-trap-skip="">
            <textarea aria-label="Terminal" />
          </div>
          <button type="button" disabled>
            Off
          </button>
          <button type="button">Last</button>
        </Modal>
      ) : null}
    </>
  );
}

// A dialog with a button that opens a second dialog on top of it.
function Stacked() {
  const [top, setTop] = useState(false);
  return (
    <Modal className="dialog" label="Under" onClose={() => {}}>
      <button type="button" onClick={() => setTop(true)}>
        Under
      </button>
      {top ? (
        <Modal className="dialog" label="Top" onClose={() => setTop(false)}>
          <button type="button">Top</button>
        </Modal>
      ) : null}
    </Modal>
  );
}

// A More menu whose item closes the menu and opens a dialog.
function MenuPage() {
  const [menu, setMenu] = useState(false);
  const [dialog, setDialog] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setMenu(true)}>
        More
      </button>
      {menu ? (
        <div role="menu">
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(false);
              setDialog(true);
            }}
          >
            Cancel card
          </button>
        </div>
      ) : null}
      {dialog ? (
        <Modal className="dialog" label="Cancel" onClose={() => setDialog(false)}>
          <button type="button">Keep</button>
        </Modal>
      ) : null}
    </>
  );
}

// An Archive dialog that a Delete dialog can take the place of.
function Swap() {
  const [step, setStep] = useState<"none" | "archive" | "delete">("none");
  return (
    <>
      <button type="button" onClick={() => setStep("archive")}>
        Archive
      </button>
      {step === "archive" ? (
        <Modal className="dialog" label="Archive" onClose={() => setStep("none")}>
          <button type="button" onClick={() => setStep("delete")}>
            Delete instead
          </button>
        </Modal>
      ) : null}
      {step === "delete" ? (
        <Modal className="dialog" label="Delete" onClose={() => setStep("none")}>
          <button type="button">Keep</button>
        </Modal>
      ) : null}
    </>
  );
}

// A sheet whose screen takes focus as it mounts, as a terminal does when a
// tap opens it to type.
function Sheet() {
  const [isOpen, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>
        Type
      </button>
      {isOpen ? (
        <Modal backdrop={null} className="sheet" label="Sheet" onClose={() => setOpen(false)}>
          <button type="button">Back</button>
          <textarea
            aria-label="Screen"
            ref={(element) => {
              element?.focus();
            }}
          />
        </Modal>
      ) : null}
    </>
  );
}

function openDialog() {
  const opener = screen.getByRole("button", { name: "Open" });
  opener.focus();
  fireEvent.click(opener);
  return opener;
}

test("it takes focus, and gives it back to its opener on close", () => {
  render(<Page />);
  const opener = openDialog();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "First" }));
  fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(opener);
});

test("a child that takes focus as it opens keeps it", () => {
  render(<Sheet />);
  fireEvent.click(screen.getByRole("button", { name: "Type" }));
  expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Screen" }));
});

test("Escape closes it with focus on the page body too", () => {
  const onClose = vi.fn();
  render(<Page onClose={onClose} />);
  openDialog();
  (document.activeElement as HTMLElement).blur();
  expect(document.activeElement).toBe(document.body);
  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("while a send is in flight, neither Escape nor the backdrop closes it", () => {
  const onClose = vi.fn();
  render(<Page onClose={onClose} dismissable={false} />);
  openDialog();
  fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
  fireEvent.keyDown(document.body, { key: "Escape" });
  fireEvent.mouseDown(document.querySelector(".dialog-backdrop") as Element);
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog")).toBeTruthy();
});

test("a click on the backdrop closes it, a click in the dialog does not", () => {
  const onClose = vi.fn();
  render(<Page onClose={onClose} />);
  openDialog();
  fireEvent.mouseDown(screen.getByRole("dialog"));
  expect(onClose).not.toHaveBeenCalled();
  fireEvent.mouseDown(document.querySelector(".dialog-backdrop") as Element);
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("Tab wraps between its first and last controls, past a terminal and a disabled one", () => {
  render(<Page />);
  openDialog();
  const first = screen.getByRole("button", { name: "First" });
  const last = screen.getByRole("button", { name: "Last" });

  // Shift+Tab on the first control goes to the last.
  expect(fireEvent.keyDown(first, { key: "Tab", shiftKey: true })).toBe(false);
  expect(document.activeElement).toBe(last);
  // Tab on the last goes to the first.
  expect(fireEvent.keyDown(last, { key: "Tab" })).toBe(false);
  expect(document.activeElement).toBe(first);
  // In between, the browser moves focus as usual.
  expect(fireEvent.keyDown(first, { key: "Tab" })).toBe(true);
});

test("the page behind it is inert while it is open, except the toast", () => {
  const { container } = render(<Page />);
  openDialog();
  const opener = screen.getByRole("button", { name: "Open" });
  expect(container.hasAttribute("inert")).toBe(false);
  expect(opener.closest("[inert]")).not.toBeNull();
  expect(document.querySelector(".toast")?.closest("[inert]")).toBeNull();

  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  expect(document.querySelector("[inert]")).toBeNull();
});

test("a stacked dialog makes the one under it inert, and Escape closes only the top one", () => {
  render(<Stacked />);
  const under = screen.getByRole("dialog", { name: "Under" });
  fireEvent.click(screen.getByRole("button", { name: "Under" }));
  expect(under.closest("[inert]")).not.toBeNull();

  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Top" })).toBeNull();
  expect(screen.getByRole("dialog", { name: "Under" })).toBeTruthy();
  expect(under.closest("[inert]")).toBeNull();
});

test("a dialog opened from a menu item gives focus back to the menu's button", () => {
  render(<MenuPage />);
  const trigger = screen.getByRole("button", { name: "More" });
  trigger.focus();
  fireEvent.click(trigger);
  const item = screen.getByRole("menuitem");
  item.focus();
  fireEvent.click(item);

  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Keep" }));
  fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
  expect(document.activeElement).toBe(trigger);
});

test("a dialog that takes another's place gives focus back to the first one's opener", () => {
  render(<Swap />);
  const opener = screen.getByRole("button", { name: "Archive" });
  opener.focus();
  fireEvent.click(opener);
  fireEvent.click(screen.getByRole("button", { name: "Delete instead" }));
  expect(screen.getByRole("dialog", { name: "Delete" })).toBeTruthy();

  fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(opener);
});

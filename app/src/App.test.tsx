import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "./App";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";

// Mock Tauri API
vi.mock("@tauri-apps/api/window", () => {
  const startDraggingMock = vi.fn().mockResolvedValue(undefined);
  return {
    getCurrentWindow: vi.fn(() => ({
      startDragging: startDraggingMock,
    })),
  };
});

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn().mockRejectedValue(new Error("not available in tests")),
}));

describe("App native dragging behavior", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("calls startDragging when clicking the header", () => {
    render(<App />);

    // In our App, the header has a <header> tag which resolves to 'banner' role.
    const header = screen.getByRole("banner");

    // Simulate pointer down directly on the header
    fireEvent.pointerDown(header);

    // Verify the mock was called
    const windowMock = getCurrentWindow();
    expect(windowMock.startDragging).toHaveBeenCalledOnce();
  });

  it("calls startDragging when clicking the sidebar empty space", () => {
    render(<App />);

    // The Projects label is inside the sidebar. We can find the sidebar container
    // by finding the text and getting its parent.
    const projectsLabel = screen.getByText("Projects");
    const sidebar = projectsLabel.parentElement!;

    fireEvent.pointerDown(sidebar);

    const windowMock = getCurrentWindow();
    expect(windowMock.startDragging).toHaveBeenCalledOnce();
  });
});

describe("App settings access", () => {
  beforeEach(() => {
    vi.mocked(listen).mockReset().mockResolvedValue(() => {});
  });

  it("opens settings on Cmd+,", () => {
    render(<App />);

    fireEvent.keyDown(window, { key: ",", metaKey: true });

    expect(screen.getByRole("heading", { name: "Settings" })).toBeDefined();
  });

  it("opens settings when the native menu emits open-settings", async () => {
    render(<App />);

    const [event, handler] = vi.mocked(listen).mock.calls[0];
    expect(event).toBe("open-settings");
    await act(async () => {
      (handler as (payload: unknown) => void)({});
    });

    expect(screen.getByRole("heading", { name: "Settings" })).toBeDefined();
  });

  it("unsubscribes even when unmounted before listen resolves", async () => {
    const unlisten = vi.fn();
    let resolveListen: (fn: () => void) => void = () => {};
    vi.mocked(listen).mockReturnValue(
      new Promise((resolve) => {
        resolveListen = resolve as (fn: () => void) => void;
      }) as any,
    );

    const { unmount } = render(<App />);
    unmount();
    await act(async () => {
      resolveListen(unlisten);
    });

    expect(unlisten).toHaveBeenCalledOnce();
  });
});

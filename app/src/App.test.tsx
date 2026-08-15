import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import App from "./App";
import { getCurrentWindow } from "@tauri-apps/api/window";

// Mock Tauri API
vi.mock("@tauri-apps/api/window", () => {
  const startDraggingMock = vi.fn().mockResolvedValue(undefined);
  return {
    getCurrentWindow: vi.fn(() => ({
      startDragging: startDraggingMock,
    })),
  };
});

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

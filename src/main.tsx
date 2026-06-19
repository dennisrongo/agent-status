import React from "react";
import { createRoot } from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { HoverPopover } from "./components/HoverPopover";
import { isTauriReady } from "./tauriReady";
import "./styles/app.css";

// The hover popover loads this same bundle in a separate window; render the
// compact preview there instead of the full app.
const isHover = isTauriReady() && getCurrentWindow().label === "hover";

const root = document.getElementById("app");
if (root) {
  createRoot(root).render(
    <React.StrictMode>{isHover ? <HoverPopover /> : <App />}</React.StrictMode>,
  );
}

import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./styles/app.css";

const root = document.getElementById("app");
if (root) {
  createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

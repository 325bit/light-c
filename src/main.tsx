import React from "react";
import { hydrateRoot } from "react-dom/client";
import "./i18n";
import App from "./App";
import { ThemeProvider, WallpaperProvider } from "./contexts";
import { requestAdminOnStartup } from "./utils/startupAdmin";
import "./App.css";

/**
 * Block WebView2's built-in browser context menu in production.
 *
 * That menu (Back / Refresh / Save as / Print / More tools / Inspect) is WebView2's built-in behavior,
 * not DOM we render; it is meaningless to the user and makes the app look like a web page.
 * Tauri 2.9's window builder does not expose wry's `with_default_context_menus`,
 * so we do it at the page level: WebView2 only shows the default menu when `contextmenu` is not cancelled.
 *
 * A few exceptions:
 * - Allow in input fields: right-click there needs paste / cut, and WebView2 shows a menu with context items,
 *   far more reasonable than the global "Back / Refresh"; `contenteditable=""` is equivalent to true, covered by :not([contenteditable="false"]).
 * - Allow when text is selected: right-click "Copy" is the most natural action; paths and logs must be copyable.
 * - Allow entirely in development: the app has no other devtools entry, and the console can only be opened via "Inspect" in the menu.
 *   If a custom context menu is added for an area later, allow it in this check too.
 */
if (import.meta.env.PROD) {
  window.addEventListener(
    "contextmenu",
    (event) => {
      const target = event.target;
      const isEditable =
        target instanceof Element &&
        target.closest('input, textarea, [contenteditable]:not([contenteditable="false"])');
      const hasSelection = !window.getSelection()?.isCollapsed;
      if (!isEditable && !hasSelection) {
        event.preventDefault();
      }
    },
    true,
  );
}

// Raise UAC before React takes over when the user asked for it in settings: the request is
// issued here so the system prompt does not pop up over an already interactive window.
requestAdminOnStartup();

// The static splash screen is painted before React, so use hydration to take over the original node and avoid replaying the animation on remount.
hydrateRoot(document.getElementById("root") as HTMLElement,
  <React.StrictMode>
    <ThemeProvider>
      <WallpaperProvider>
        <App />
      </WallpaperProvider>
    </ThemeProvider>
  </React.StrictMode>
);

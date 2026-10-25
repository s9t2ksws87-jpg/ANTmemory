# ANTmemory Viewer

ANTmemory Viewer is a local, read-only web interface for inspecting the SQLite memory databases used by ANTmemory.

It can browse workspaces, search memories, inspect lifecycle history, review visibility/usage metadata, and show recall reasons recorded by the recall log.

---

## Features

- Automatically discovers workspace databases under `stores/`.
- FTS5 search with Japanese trigram support and LIKE fallback.
- Filters by agent, importance, tag, and lifecycle history.
- Shows supersede/retract/restore history and visibility, usage, and recall information.
- Opens workspace databases read-only.
- Uses DOM APIs for data-derived text; stored HTML/JavaScript is displayed as text rather than executed.
- Has no runtime npm dependencies; it uses Node.js 24 built-ins (`node:http`, `node:sqlite`).

---

## Start

### Windows

Double-click `start-viewer.bat`, or run:

```powershell
cd <ANTMEMORY_ROOT>\viewer
.\start-viewer.bat
```

### Any platform

```powershell
cd <ANTMEMORY_ROOT>\viewer
node server.mjs
```

Then open [http://localhost:3737](http://localhost:3737).

By default the server binds only to `127.0.0.1`. Do not expose it to an untrusted network. ANTmemory does not provide authentication for the Viewer.

---

## Layout

```text
viewer/
├── start-viewer.bat    Windows launcher
├── server.mjs          Node.js HTTP/API server
├── README.md           this document
└── public/
    ├── index.html      Viewer UI
    ├── styles.css      Viewer styles
    └── app.js          browser-side state and rendering
```

`ANT_MEMORY_ROOT` can be used to point the Viewer at another ANTmemory root. When omitted, the Viewer resolves the repository root from its own location.

# Orochi

A Swiss-army knife of small tools for my work workflows, built as a single
cross-platform desktop app with [Tauri](https://tauri.app). Each tool lives in
its own tab in the left sidebar.

## Tools

### Counter

A counter with a settable hotkey.

- **Count up** — press the hotkey (default `Space`).
- **Rebind** — click the hotkey chip, then press any key (`Esc` cancels).
- **Reset** — set the counter back to `0`.

The count and hotkey persist via `localStorage`.

### GIF Maker

Capture frames from a region of your screen and stitch them into a GIF.

- **Select Region** — drag a box over the screen to choose the capture area
  (leave it unset to capture the full primary monitor).
- **Capture hotkey** — press it (default `F8`, rebindable) to grab one frame of
  the region. It's a **global** hotkey, so it fires even when Orochi isn't the
  focused window — capture other apps while they're in front. You can also click
  **Capture frame**.
- **Output Folder** — pick where GIFs are written (remembered between runs).
- **Frame delay / Max width** — playback delay per frame and an optional
  downscale cap to keep file sizes reasonable (`0` = original size).
- **Generate GIF** — encodes all captured frames and saves to the output folder.

Frames live in memory until you **Generate** or **Clear**.

> **Windows note:** capturing or hotkey-triggering over an app running *as
> administrator* requires running Orochi as administrator too (Windows privilege
> isolation). Protected surfaces (the UAC prompt, lock screen, some DRM video)
> can never be captured.

### Quality

A reviewer-mistake dashboard for the labeling workflow. As you open clips in the
labeling app, a companion **Tampermonkey userscript** snoops each clip's reviewer
comments and forwards them to Orochi over a loopback connection; Orochi parses
them against a 14-entry mistake taxonomy and shows:

- **Mistake Tally** — a worst-first histogram of every mistake across all clips,
  with major/minor severity badges.
- **Self-Review Checklist** — the top-N weakest categories for the current clip
  as tickable items (resets per clip; ticks persist locally).
- **Connection panel** — the bridge URL + shared token to put in the userscript.

Comment data is stored at `%APPDATA%\com.bangojetty.orochi\quality.json`; the
checklist/top-N UI state lives in `localStorage`.

#### The bridge (how comments reach Orochi)

The hook is a userscript, not an extension, because managed Chrome blocks
unverified/unpacked extensions — but Tampermonkey is already permitted. The
script lives at [`userscript/orochi-quality-hook.user.js`](userscript/orochi-quality-hook.user.js).

1. Install Tampermonkey, then add the script (paste its contents into a new
   userscript, or drag the file in).
2. **Edit the two `@match` lines** to your real labeling-app host(s) — they ship
   as placeholders so nothing private is committed.
3. Make sure Orochi is running, then open a clip. The script POSTs the clip's
   comments to `http://127.0.0.1:47800/ingest` and the Quality tab updates live.

Why it works where a plain `fetch` wouldn't: the script hooks the page's *real*
`fetch`/XHR (via `unsafeWindow`) to see the `/api/internal/3d/videos/<id>`
response, and forwards it with `GM_xmlhttpRequest`, which is exempt from the
HTTPS page's mixed-content/CORS rules. The listener binds `127.0.0.1`, so traffic
never leaves the machine, and every request must carry a shared `X-Orochi-Token`.
Override the port/token with the `OROCHI_PORT` / `OROCHI_TOKEN` env vars (Orochi)
and the Tampermonkey menu (userscript).

## Development

Prerequisites: [Rust](https://rustup.rs), Node, and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

```sh
npm install        # install the Tauri CLI
npm run dev        # run the app with hot-reload
npm run build      # produce a packaged installer / executable
```

The frontend is plain HTML/CSS/JS in `ui/`; the native backend is in
`src-tauri/`. No frontend build step or framework.

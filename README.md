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

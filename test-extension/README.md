# Orochi Hook Test (throwaway probe)

A minimal MV3 extension that proves — or disproves — the three capabilities the
real Orochi quality bridge depends on, **before** we build the whole thing:

1. **Unpacked load** — does your managed Chrome even allow Load Unpacked?
2. **MAIN-world fetch hook** — can a content script patch the page's *real*
   `window.fetch` (the one the labeling app uses)? This needs `world: "MAIN"`.
3. **Localhost reachability** — can the background service worker reach
   `http://127.0.0.1` (the Orochi side of the bridge)?

It runs on **every** site (`<all_urls>`), so you can test it anywhere — you do
**not** need the labeling app for this probe.

## How to test

### Step 1 — Load it (tests capability #1)
1. Open `chrome://extensions`.
2. Toggle **Developer mode** on (top-right).
   - If that toggle is greyed out or policy-locked, the extension path is
     blocked → we'll use the Tampermonkey userscript instead. Stop here and tell me.
3. Click **Load unpacked** and select this `test-extension` folder.
   - If it loads without a policy error, **capability #1 passes.** ✅

### Step 2 — Visit any HTTPS page (tests capability #2)
1. Open any normal site, e.g. `https://example.com`, and **reload**.
2. You should see a green **“🐍 Orochi hook active (MAIN world)”** badge
   top-right of the page.
3. Open DevTools → Console. You should see `[orochi-test] fetch: …` lines as the
   page makes requests.
   - Badge + fetch logs = **capability #2 passes.** ✅ (This is the make-or-break
     one for MV3.)

### Step 3 — Check the localhost path (tests capability #3)
1. On `chrome://extensions`, find **Orochi Hook Test** → click **service
   worker** (the blue link) to open its console.
2. Reload your test page so a fetch sample gets relayed.
3. In the service-worker console you'll see one of:
   - `localhost REACHABLE -- HTTP 501` (or any status) → **passes**, even
     without a server running, this means the request reached the port.
   - `localhost NOT reached: Failed to fetch` → either nothing is listening
     **or** it's blocked. To tell them apart, run the optional listener below
     and retry — if it *still* fails with the listener running, it's blocked.

**Optional listener** (to get a definitive REACHABLE result) — run in any terminal:

```sh
python -m http.server 47800
```

Then reload the page and recheck the service-worker console. A Python listener
answers POST with HTTP 501, which still proves reachability.

## What to report back
- Did Developer mode / Load unpacked work? (cap #1)
- Did the green badge + `[orochi-test] fetch:` logs appear? (cap #2)
- What did the service-worker console say about localhost? (cap #3)

If all three pass, we build the real extension. If #1 or #2 fail, we pivot to
the Tampermonkey userscript (which is already proven on your machine via MadCat).

> Throwaway — delete this folder once we've decided the path.

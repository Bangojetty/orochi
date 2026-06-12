// ==UserScript==
// @name         Orochi Quality Hook
// @namespace    orochi
// @version      0.1.0
// @description  Snoops the labeling app's per-clip reviewer comments and forwards them to the Orochi desktop app over localhost. All parsing/taxonomy lives in Orochi — this script just hooks fetch/XHR and POSTs the raw comments.
// @author       bangojetty
// @match        https://YOUR-LABELING-APP.example.com/*
// @match        https://YOUR-OTHER-FRAME.example.com/*
// @grant        unsafeWindow
// @grant        GM_xmlhttpRequest
// @grant        GM_getValue
// @grant        GM_setValue
// @grant        GM_registerMenuCommand
// @connect      127.0.0.1
// @connect      localhost
// @run-at       document-start
// ==/UserScript==
//
// SETUP: replace the two @match lines above with your real labeling-app host(s)
// in the Tampermonkey editor (Tampermonkey → this script → save). The host is
// intentionally not committed. Nothing else needs editing for the default port
// (47800) and token; change them via the Tampermonkey menu if you change them
// in Orochi.

(function () {
  "use strict";

  // Patch the PAGE's real fetch/XHR, not Tampermonkey's wrapper realm. With any
  // @grant present, TM isolates us; unsafeWindow reaches the page so the React
  // app's own requests actually flow through our hook. (Same trick as MadCat.)
  var page = (typeof unsafeWindow !== "undefined" && unsafeWindow) ? unsafeWindow : window;
  var TAG = "[orochi-hook]";

  // ── Config (per-machine overrides live in Tampermonkey storage) ────────────
  var DEFAULT_PORT = 47800;
  var DEFAULT_TOKEN = "orochi-local-7Q2vXm"; // shared secret; must match Orochi
  function cfgPort() {
    try { return parseInt(GM_getValue("orochi:port", DEFAULT_PORT), 10) || DEFAULT_PORT; }
    catch (e) { return DEFAULT_PORT; }
  }
  function cfgToken() {
    try { return GM_getValue("orochi:token", DEFAULT_TOKEN) || DEFAULT_TOKEN; }
    catch (e) { return DEFAULT_TOKEN; }
  }

  // ── Comment extraction (mirrors MadCat's tolerant response handling) ───────
  function clipIdFromVideoUrl(url) {
    var m = /\/videos\/([^/?#]+)/.exec(String(url || ""));
    return m ? decodeURIComponent(m[1]) : null;
  }
  // The API nests comments under a `comments` object holding one array of items,
  // each with a `comment` string. We take the first array we find and also
  // tolerate `comments` being the array itself.
  function extractCommentStrings(json) {
    if (!json || typeof json !== "object") return [];
    var container = json.comments;
    if (!container) return [];
    var list = null;
    if (Array.isArray(container)) {
      list = container;
    } else if (typeof container === "object") {
      for (var k in container) {
        if (Array.isArray(container[k])) { list = container[k]; break; }
      }
    }
    if (!Array.isArray(list)) return [];
    var out = [];
    for (var i = 0; i < list.length; i++) {
      var item = list[i];
      if (item && typeof item === "object" && typeof item.comment === "string") out.push(item.comment);
      else if (typeof item === "string") out.push(item);
    }
    return out;
  }

  // ── Forward to Orochi over localhost ───────────────────────────────────────
  // GM_xmlhttpRequest runs in the extension's privileged context, so it is NOT
  // subject to the HTTPS page's mixed-content block or CORS — the reason a plain
  // fetch('http://127.0.0.1') from the page would fail.
  var lastSig = {}; // clipId → signature, so we don't re-POST identical payloads
  function send(clipId, comments) {
    var sig = comments.length + ":" + comments.join("").length;
    if (lastSig[clipId] === sig) return; // unchanged since the last successful look
    lastSig[clipId] = sig;
    var body = JSON.stringify({ clipId: clipId, comments: comments, sentAt: Date.now() });
    try {
      GM_xmlhttpRequest({
        method: "POST",
        url: "http://127.0.0.1:" + cfgPort() + "/ingest",
        headers: { "Content-Type": "application/json", "X-Orochi-Token": cfgToken() },
        data: body,
        timeout: 4000,
        onload: function (res) {
          console.log(TAG, "sent clip", clipId, "(" + comments.length + " comments) →", res.status);
        },
        onerror: function () {
          delete lastSig[clipId]; // allow a retry on the next page fetch
          console.warn(TAG, "Orochi not reachable on port " + cfgPort() + " — is the app running?");
        },
        ontimeout: function () {
          delete lastSig[clipId];
          console.warn(TAG, "Orochi POST timed out");
        }
      });
    } catch (e) {
      delete lastSig[clipId];
      console.warn(TAG, "GM_xmlhttpRequest failed:", e);
    }
  }

  function ingest(clipId, json) {
    if (!clipId || !json || typeof json !== "object" || json.comments == null) return;
    send(clipId, extractCommentStrings(json));
  }

  // ── Hook the page's real fetch ─────────────────────────────────────────────
  var origFetch = page.fetch;
  if (typeof origFetch === "function") {
    page.fetch = function (input) {
      var url = "";
      try { url = (typeof input === "string") ? input : (input && input.url) || ""; } catch (e) {}
      var p = origFetch.apply(this, arguments);
      try {
        if (/\/api\/internal\/3d\/videos\//.test(url)) {
          var clipId = clipIdFromVideoUrl(url);
          if (clipId) {
            // Snoop on a SEPARATE chain (clone the body) so the page still gets
            // the original response untouched.
            p.then(function (res) {
              try {
                res.clone().json()
                  .then(function (json) { ingest(clipId, json); })
                  .catch(function () { /* not JSON / unexpected shape */ });
              } catch (e) {}
            }).catch(function () {});
          }
        }
      } catch (e) {}
      return p;
    };
    console.log(TAG, "fetch hook installed on", (page === window ? "window (wrapper)" : "unsafeWindow (page)"));
  }

  // ── Hook XHR too (cheap belt-and-suspenders) ───────────────────────────────
  var XHR = page.XMLHttpRequest;
  if (XHR && XHR.prototype) {
    var origOpen = XHR.prototype.open;
    var origSend = XHR.prototype.send;
    XHR.prototype.open = function (method, url) {
      this.__orochi_url = url;
      return origOpen.apply(this, arguments);
    };
    XHR.prototype.send = function () {
      var self = this;
      try {
        if (/\/api\/internal\/3d\/videos\//.test(String(self.__orochi_url || ""))) {
          self.addEventListener("load", function () {
            try {
              var clipId = clipIdFromVideoUrl(self.__orochi_url);
              if (clipId) ingest(clipId, JSON.parse(self.responseText));
            } catch (e) {}
          });
        }
      } catch (e) {}
      return origSend.apply(this, arguments);
    };
  }

  // ── Tampermonkey menu: change the localhost port / token if Orochi's differ ─
  try {
    if (typeof GM_registerMenuCommand === "function") {
      GM_registerMenuCommand("Orochi: set port", function () {
        var v = prompt("Orochi localhost port", String(cfgPort()));
        if (v) GM_setValue("orochi:port", parseInt(v, 10) || DEFAULT_PORT);
      });
      GM_registerMenuCommand("Orochi: set token", function () {
        var v = prompt("Orochi shared token (must match the app)", cfgToken());
        if (v) GM_setValue("orochi:token", v);
      });
    }
  } catch (e) {}

  console.log(TAG, "loaded — forwarding /videos comments to http://127.0.0.1:" + cfgPort());
})();

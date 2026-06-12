// Runs in the PAGE's real JS realm (manifest world: "MAIN"), so it patches the
// very same window.fetch the site's app uses -- the exact capability the real
// Orochi quality bridge will need to snoop the /videos response. chrome.* APIs
// are NOT available here, so this talks to the isolated bridge via DOM events.
(function () {
  "use strict";
  var TAG = "[orochi-test]";
  console.log(TAG, "inject.js loaded in MAIN world:", location.href);

  // Visible, unmissable proof the MAIN-world injection actually happened.
  function badge() {
    try {
      if (document.getElementById("orochi-test-badge")) return;
      var el = document.createElement("div");
      el.id = "orochi-test-badge";
      el.textContent = "🐍 Orochi hook active (MAIN world)";
      el.style.cssText = [
        "position:fixed", "top:10px", "right:10px", "z-index:2147483647",
        "background:#0f1115", "color:#00d4a0",
        "font:12px/1.4 system-ui,sans-serif",
        "padding:6px 10px", "border:1px solid #00d4a0", "border-radius:8px",
        "box-shadow:0 2px 10px rgba(0,0,0,.4)", "pointer-events:none"
      ].join(";");
      (document.body || document.documentElement).appendChild(el);
    } catch (e) { /* pre-body race; the DOMContentLoaded retry covers it */ }
  }
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", badge);
  } else {
    badge();
  }

  // Patch fetch: log every request, and relay a throttled sample to the isolated
  // bridge so the full MAIN -> ISOLATED -> service-worker -> localhost chain is
  // exercised end to end (this is the same path the real bridge will use).
  var origFetch = window.fetch;
  var lastRelay = 0;
  window.fetch = function (input) {
    var url = "";
    try { url = (typeof input === "string") ? input : (input && input.url) || ""; } catch (e) {}
    console.log(TAG, "fetch:", url);
    try {
      var now = (window.performance && performance.now) ? performance.now() : 0;
      if (now - lastRelay > 2000) {
        lastRelay = now;
        window.dispatchEvent(new CustomEvent("orochi-test-sample", { detail: { url: url } }));
      }
    } catch (e) {}
    return origFetch.apply(this, arguments);
  };
  console.log(TAG, "window.fetch patched in MAIN world");
})();

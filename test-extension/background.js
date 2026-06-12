// Background service worker. Receives relayed fetch samples and tests whether
// the extension can reach a localhost HTTP endpoint -- the Orochi side of the
// real bridge. Service workers are NOT subject to the page's mixed-content
// rules, so an HTTPS page can drive an http://127.0.0.1 request through here
// (this is exactly why the real bridge will POST from the SW, not the page).
var TARGET = "http://127.0.0.1:47800/orochi-test";

chrome.runtime.onInstalled.addListener(function () {
  console.log("[orochi-test/sw] installed -- service worker alive");
});

chrome.runtime.onMessage.addListener(function (msg, sender, sendResponse) {
  console.log("[orochi-test/sw] message from", (sender && sender.url) || "?", msg);
  fetch(TARGET, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ok: true, sample: msg, ts: Date.now() })
  })
    .then(function (res) {
      // ANY HTTP status (even 501 from a dumb listener) proves we REACHED
      // localhost -- i.e. the path is not policy-blocked.
      console.log("[orochi-test/sw] localhost REACHABLE -- HTTP", res.status);
      sendResponse({ localhost: "reachable", status: res.status });
    })
    .catch(function (err) {
      // A network error here means either nothing is listening on the port OR
      // the request was blocked. Run the optional listener (see README) to tell
      // them apart: with it running, a network error == blocked.
      console.log("[orochi-test/sw] localhost NOT reached:", err && err.message);
      sendResponse({ localhost: "unreachable", error: String(err && err.message) });
    });
  return true; // keep the channel open for the async sendResponse
});

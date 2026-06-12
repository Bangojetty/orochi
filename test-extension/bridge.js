// Runs in the extension's ISOLATED content-script world. inject.js (MAIN world)
// can patch the page's fetch but can't use chrome.* APIs; this bridge listens
// for its DOM events and relays them to the background service worker -- the
// standard MV3 pattern for getting page-realm data into the extension.
(function () {
  "use strict";
  var TAG = "[orochi-test/bridge]";
  console.log(TAG, "bridge loaded (isolated world):", location.href);

  window.addEventListener("orochi-test-sample", function (e) {
    var detail = (e && e.detail) || {};
    try {
      chrome.runtime.sendMessage(
        { type: "sample", url: detail.url, page: location.href },
        function (resp) {
          if (chrome.runtime.lastError) {
            console.log(TAG, "sendMessage error:", chrome.runtime.lastError.message);
          } else {
            console.log(TAG, "service worker replied:", resp);
          }
        }
      );
    } catch (err) {
      console.log(TAG, "relay failed:", err);
    }
  });
})();

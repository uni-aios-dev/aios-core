/* AIOS injected Web3 provider (EIP-1193 shaped).
 *
 * Runs at document start in the native WebView (wry `window.ipc.postMessage`)
 * and in the embedded Boa browser (`window.__aiosWeb3Invoke`). The Rust host
 * resolves requests through `window.__aiosWeb3Resolve(id, ok, value)`.
 *
 * Sensitive actions (account access and message signing) show an in-page
 * confirmation panel before the request is sent to the host. The panel is a
 * usability safeguard, not a security boundary: a page that executes script
 * can call the transport directly. The native wallet must therefore keep its
 * own policy for dangerous methods.
 */
(function (global) {
  "use strict";
  if (global.ethereum && global.ethereum.__aiosProvider) return;

  var pending = {};
  var seq = 1;
  var listeners = {};
  var state = { accounts: [], chainId: null };

  function emit(event, payload) {
    var list = listeners[event] || [];
    for (var i = 0; i < list.length; i++) {
      try { list[i](payload); } catch (e) { console.warn("[aios-web3] listener", e); }
    }
  }

  function fail(code, message) {
    var error = new Error(String(message || "Web3 request failed"));
    error.code = code;
    return error;
  }

  function transport(request) {
    return new Promise(function (resolve, reject) {
      var id = "aios-web3-" + seq++;
      pending[id] = { resolve: resolve, reject: reject };
      var wire;
      try {
        wire = JSON.stringify({
          id: id,
          method: String(request.method || ""),
          params: Array.isArray(request.params) ? request.params : [],
          origin: (global.location && global.location.origin) || "null",
          embedded: global.__aiosEmbedded === true
        });
      } catch (e) {
        delete pending[id];
        reject(e);
        return;
      }
      try {
        if (global.ipc && typeof global.ipc.postMessage === "function") {
          global.ipc.postMessage(wire);
        } else if (typeof global.__aiosWeb3Invoke === "function") {
          global.__aiosWeb3Invoke(wire);
        } else {
          delete pending[id];
          reject(fail(32000, "AIOS Web3 bridge is unavailable"));
        }
      } catch (e) {
        delete pending[id];
        reject(e);
      }
    });
  }

  global.__aiosWeb3Resolve = function (id, ok, value) {
    var entry = pending[id];
    if (!entry) return;
    delete pending[id];
    if (ok) {
      entry.resolve(value);
    } else {
      var code = value && value.code ? value.code : 32000;
      var message = value && value.message ? value.message : "Web3 request failed";
      entry.reject(fail(code, message));
    }
  };

  function confirmDialog(title, detail) {
    return new Promise(function (resolve) {
      if (global.__aiosEmbedded) { resolve(true); return; }
      var doc = global.document;
      if (!doc || !doc.body) { resolve(false); return; }

      var overlay = doc.createElement("div");
      overlay.style.cssText = "position:fixed;inset:0;z-index:2147483647;display:flex;" +
        "align-items:center;justify-content:center;background:rgba(4,8,16,.78);" +
        "font:14px/1.5 system-ui,sans-serif;color:#e8eef7;";
      var card = doc.createElement("div");
      card.style.cssText = "max-width:520px;width:min(92vw,520px);padding:22px;border-radius:12px;" +
        "background:#111927;border:1px solid #2a3b55;box-shadow:0 18px 48px rgba(0,0,0,.6);";
      var head = doc.createElement("div");
      head.style.cssText = "font-size:16px;font-weight:700;margin-bottom:10px;";
      head.textContent = "AIOS Wallet · " + title;
      var body = doc.createElement("div");
      body.style.cssText = "word-break:break-all;color:#9fb3cc;margin-bottom:18px;";
      body.textContent = detail;
      var actions = doc.createElement("div");
      actions.style.cssText = "display:flex;justify-content:flex-end;gap:10px;";
      var reject = doc.createElement("button");
      reject.textContent = "Reject";
      reject.style.cssText = "padding:8px 16px;border-radius:6px;border:1px solid #3a4b63;background:#1b2434;color:#e8eef7;cursor:pointer;";
      var approve = doc.createElement("button");
      approve.textContent = "Approve";
      approve.style.cssText = "padding:8px 16px;border-radius:6px;border:0;background:#3d7dff;color:#fff;cursor:pointer;";
      actions.appendChild(reject);
      actions.appendChild(approve);
      card.appendChild(head);
      card.appendChild(body);
      card.appendChild(actions);
      overlay.appendChild(card);

      function finish(value) {
        if (overlay.parentNode) overlay.parentNode.removeChild(overlay);
        resolve(value);
      }
      reject.onclick = function () { finish(false); };
      approve.onclick = function () { finish(true); };
      doc.body.appendChild(overlay);
      approve.focus && approve.focus();
    });
  }

  function sync(result, method) {
    if (method === "eth_accounts" || method === "eth_requestAccounts") {
      if (Array.isArray(result) && result.length) {
        state.accounts = result;
        emit("accountsChanged", result);
      }
    }
    if (method === "eth_chainId" || method === "wallet_switchEthereumChain") {
      if (typeof result === "string" && result) {
        state.chainId = result;
        emit("chainChanged", result);
      }
    }
    return result;
  }

  function request(args) {
    var arg = args || {};
    var method = String(arg.method || "");
    var params = Array.isArray(arg.params) ? arg.params : [];
    var origin = (global.location && global.location.origin) || "null";

    if (method === "eth_requestAccounts") {
      return confirmDialog("Connect", "Allow " + origin + " to read your AIOS wallet address?")
        .then(function (approved) {
          if (!approved) throw fail(4001, "User rejected the request");
          return transport({ method: "wallet_approveAccountAccess", params: [origin] });
        })
        .then(function () { return transport({ method: method, params: params }); })
        .then(function (result) { return sync(result, method); });
    }

    if (method === "personal_sign" || method === "eth_sign") {
      var message = String(params[0] || params[1] || "");
      return confirmDialog("Sign message", message.slice(0, 240))
        .then(function (approved) {
          if (!approved) throw fail(4001, "User rejected the request");
          return transport({ method: "wallet_approveSignature", params: [message, origin] })
            .then(function (token) {
              return transport({ method: method, params: [token, message].concat(params.slice(1)) });
            });
        });
    }

    return transport({ method: method, params: params })
      .then(function (result) { return sync(result, method); });
  }

  var provider = {
    __aiosProvider: true,
    isAIOS: true,
    isMetaMask: true,
    isConnected: function () { return true; },
    autoRefreshOnNetworkChange: false,
    request: request,
    on: function (event, handler) {
      if (!listeners[event]) listeners[event] = [];
      listeners[event].push(handler);
      return provider;
    },
    removeListener: function (event, handler) {
      var list = listeners[event] || [];
      var i = list.indexOf(handler);
      if (i >= 0) list.splice(i, 1);
      return provider;
    },
    emit: emit
  };
  Object.defineProperty(provider, "selectedAddress", {
    get: function () { return state.accounts.length ? state.accounts[0] : null; }
  });
  Object.defineProperty(provider, "chainId", {
    get: function () { return state.chainId || "0x1"; }
  });

  global.ethereum = provider;
  if (!global.web3) {
    global.web3 = {
      currentProvider: provider,
      givenProvider: provider,
      utils: {
        toChecksumAddress: function (address) { return String(address || "").toLowerCase(); },
        fromWei: function (value) { return Number(value || 0) / 1e18; },
        toWei: function (value) { return String(Math.round(Number(value || 0) * 1e18)); }
      }
    };
  }
})(typeof globalThis !== "undefined" ? globalThis : window);

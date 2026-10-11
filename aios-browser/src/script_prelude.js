/* AIOS browser script prelude — a minimal DOM over globalThis.__dom.
 *
 * The Rust side injects three globals before this file runs:
 *   __dom    — the page tree: {tag, attrs:[[k,v]], children:[], text}
 *   __meta   — {url, title, ua, nav, cookie, local_storage, ...}
 *   __console — string log lines collected by the console shim.
 *
 * All nodes are hydrated with non-enumerable accessors (textContent,
 * innerHTML, appendChild, ...), so JSON.stringify(node) still yields the
 * plain DomNode shape Rust deserialises.
 */
(() => {
  "use strict";

  const VOID = new Set(["area","base","br","col","embed","hr","img","input","link","meta","param","source","track","wbr"]);
  const RAW = new Set(["script", "style"]);
  const TEXT = "#text";

  globalThis.__console = globalThis.__console || [];

  const note = (msg) => {
    if (globalThis.__console.length < 400) globalThis.__console.push(msg);
  };
  const errStr = (e) => {
    try {
      if (e && e.message) {
        const msg = String(e.message);
        const stack = e.stack ? String(e.stack) : "";
        if (!stack || stack.includes(msg)) return stack || msg;
        return msg + "\n" + stack;
      }
      return String(e);
    } catch (_) {
      return "<unprintable error>";
    }
  };
  const fmtArg = (a) => {
    try {
      if (typeof a === "string") return a;
      if (a === null) return "null";
      if (a === undefined) return "undefined";
      if (typeof a === "object" && a.message) return String(a.stack || a.message);
      const j = JSON.stringify(a);
      return j === undefined ? String(a) : j;
    } catch (_) {
      return String(a);
    }
  };
  const fmtArgs = (args) => Array.prototype.map.call(args, fmtArg).join(" ");

  /* ---- console -------------------------------------------------------- */
  const mkLog = (level) => (...args) => {
    const line = level ? "[" + level + "] " + fmtArgs(args) : fmtArgs(args);
    note(line.length > 1000 ? line.slice(0, 1000) + "…" : line);
  };
  globalThis.console = {
    log: mkLog(""),
    info: mkLog("info"),
    warn: mkLog("warn"),
    error: mkLog("error"),
    debug: mkLog("debug"),
    trace: mkLog("trace"),
    dir: mkLog(""),
    table: mkLog(""),
    group: mkLog(""),
    groupEnd: () => {},
    time: () => {},
    timeEnd: () => {},
    assert: (cond, ...rest) => { if (!cond) mkLog("assert")(...rest); },
    count: () => {},
    clear: () => { globalThis.__console.length = 0; },
  };

  /* ---- events: listener storage, dispatch, constructors ----------------- */
  const ensureBag = (obj) => {
    if (!obj.__ev) Object.defineProperty(obj, "__ev", { value: {}, enumerable: false, writable: true, configurable: true });
    return obj.__ev;
  };
  const addLis = (obj, type, fn) => {
    if (typeof fn !== "function") return;
    const k = String(type);
    const bag = ensureBag(obj);
    (bag[k] = bag[k] || []).push(fn);
  };
  const removeLis = (obj, type, fn) => {
    const list = obj.__ev && obj.__ev[String(type)];
    if (list) { const i = list.indexOf(fn); if (i >= 0) list.splice(i, 1); }
  };
  const mkEvent = (type, target) => ({
    type: String(type),
    target: target || null,
    currentTarget: null,
    bubbles: true,
    cancelable: true,
    defaultPrevented: false,
    timeStamp: Date.now(),
    preventDefault() { this.defaultPrevented = true; },
    stopPropagation() {},
    stopImmediatePropagation() {},
    composedPath() { return [this.target]; },
  });
  const asEvent = (evt, target) => {
    if (evt && typeof evt === "object") { if (!evt.target) evt.target = target; return evt; }
    return mkEvent(String(evt), target);
  };
  const callHandler = (fn, self, evt, label) => {
    try { fn.call(self, evt); }
    catch (e) { note("[js] " + label + " handler error: " + errStr(e)); }
  };
  const fireAt = (obj, evt) => {
    const list = obj.__ev && obj.__ev[evt.type];
    if (list) for (const fn of list.slice()) callHandler(fn, obj, evt, evt.type);
    const prop = obj["on" + evt.type];
    if (typeof prop === "function") callHandler(prop, obj, evt, "on" + evt.type);
    return !evt.defaultPrevented;
  };
  globalThis.Event = class Event {
    constructor(type, init) {
      init = init || {};
      this.type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.target = null;
      this.currentTarget = null;
      this.defaultPrevented = false;
      this.timeStamp = Date.now();
    }
    preventDefault() { if (this.cancelable) this.defaultPrevented = true; }
    stopPropagation() {}
    stopImmediatePropagation() {}
    get isTrusted() { return true; }
  };
  globalThis.CustomEvent = class CustomEvent extends globalThis.Event {
    constructor(type, init) {
      super(type, init);
      this.detail = init && init.detail !== undefined ? init.detail : null;
    }
  };

  /* ---- raw node helpers ------------------------------------------------ */
  const meta = () => globalThis.__meta;
  const getAttr = (n, k) => {
    for (const p of n.attrs) if (p[0] === k) return p[1];
    return null;
  };
  const setAttr = (n, k, v) => {
    const s = String(v);
    for (const p of n.attrs) if (p[0] === k) { p[1] = s; return; }
    n.attrs.push([k, s]);
  };
  const delAttr = (n, k) => {
    for (let i = 0; i < n.attrs.length; i++) if (n.attrs[i][0] === k) { n.attrs.splice(i, 1); return; }
  };
  const mkNode = (tag, text) => ({ tag, attrs: [], children: [], text: text || "" });

  const escText = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const escAttr = (s) => escText(s).replace(/"/g, "&quot;");

  const serialize = (node) => {
    if (node.tag === TEXT) return escText(node.text);
    if (node.tag === "document") return node.children.map(serialize).join("");
    if (node.tag[0] === "#") return "";
    let out = "<" + node.tag;
    for (const p of node.attrs) out += " " + p[0] + '="' + escAttr(p[1]) + '"';
    if (VOID.has(node.tag)) return out + ">";
    out += ">";
    if (RAW.has(node.tag)) {
      for (const c of node.children) if (c.tag === TEXT) out += c.text;
    } else {
      out += node.children.map(serialize).join("");
    }
    return out + "</" + node.tag + ">";
  };

  const parseAttrs = (s) => {
    const attrs = [];
    const re = /([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s]+)))?/g;
    let m;
    while ((m = re.exec(s))) {
      const v = m[2] !== undefined ? m[2] : m[3] !== undefined ? m[3] : m[4] !== undefined ? m[4] : "";
      attrs.push([m[1], v]);
    }
    return attrs;
  };

  /* Minimal HTML fragment parser: tags, attrs, text, raw-text elements. */
  const parseFragment = (src) => {
    const root = mkNode("#root");
    const stack = [root];
    const lower = String(src).toLowerCase();
    const pushText = (t) => {
      if (!t) return;
      stack[stack.length - 1].children.push(mkNode(TEXT, t));
    };
    let i = 0;
    const s = String(src);
    while (i < s.length) {
      const lt = s.indexOf("<", i);
      if (lt < 0) { pushText(s.slice(i)); break; }
      if (lt > i) pushText(s.slice(i, lt));
      if (s.startsWith("<!--", lt)) {
        const end = s.indexOf("-->", lt + 4);
        i = end < 0 ? s.length : end + 3;
        continue;
      }
      if (s.startsWith("<!", lt) || s.startsWith("<?", lt)) {
        const gt = s.indexOf(">", lt);
        i = gt < 0 ? s.length : gt + 1;
        continue;
      }
      if (s.startsWith("</", lt)) {
        const gt = s.indexOf(">", lt);
        if (gt < 0) break;
        const name = s.slice(lt + 2, gt).trim().toLowerCase();
        for (let k = stack.length - 1; k >= 1; k--) {
          if (stack[k].tag === name) { stack.length = k; break; }
        }
        i = gt + 1;
        continue;
      }
      let gt = -1, quote = "";
      for (let j = lt + 1; j < s.length; j++) {
        const c = s[j];
        if (quote) { if (c === quote) quote = ""; }
        else if (c === '"' || c === "'") quote = c;
        else if (c === ">") { gt = j; break; }
      }
      if (gt < 0) { pushText(s.slice(lt)); break; }
      const inner = s.slice(lt + 1, gt);
      const selfClose = inner.endsWith("/");
      const body = selfClose ? inner.slice(0, -1) : inner;
      const m = body.match(/^\s*([^\s/>]+)/);
      if (!m) { i = gt + 1; continue; }
      const tag = m[1].toLowerCase();
      const node = mkNode(tag);
      node.attrs = parseAttrs(body.slice(m[0].length));
      stack[stack.length - 1].children.push(node);
      i = gt + 1;
      if (selfClose || VOID.has(tag)) continue;
      if (RAW.has(tag)) {
        const close = lower.indexOf("</" + tag, i);
        if (close >= 0) {
          if (close > i) node.children.push(mkNode(TEXT, s.slice(i, close)));
          i = close;
        } else {
          node.children.push(mkNode(TEXT, s.slice(i)));
          i = s.length;
        }
      } else {
        stack.push(node);
      }
    }
    return root.children;
  };

  /* ---- selectors: tag, #id, .class, compounds, descendants, groups ----- */
  const matchSimple = (node, simple) => {
    if (node.tag[0] === "#") return false;
    let rest = simple;
    let any = false;
    const tagM = rest.match(/^[a-zA-Z][\w-]*/);
    if (tagM) {
      if (node.tag !== tagM[0].toLowerCase()) return false;
      rest = rest.slice(tagM[0].length);
      any = true;
    }
    const re = /([#.])([\w-]+)/g;
    let m;
    while ((m = re.exec(rest))) {
      any = true;
      if (m[1] === "#") {
        if (getAttr(node, "id") !== m[2]) return false;
      } else {
        const cls = getAttr(node, "class") || "";
        if (!cls.split(/\s+/).includes(m[2])) return false;
      }
    }
    return any;
  };

  const queryAll = (root, sel) => {
    const chains = String(sel)
      .split(",")
      .map((p) => p.trim().split(/\s+/).filter(Boolean))
      .filter((c) => c.length);
    const all = [];
    const parentOf = new Map();
    const walk = (n) => {
      all.push(n);
      for (const c of n.children) { parentOf.set(c, n); walk(c); }
    };
    if (root.children) walk(root);
    const out = [];
    for (const node of all) {
      if (node === root) continue;
      for (const chain of chains) {
        let idx = chain.length - 1;
        let n = node;
        while (n && idx >= 0) {
          if (matchSimple(n, chain[idx])) idx--;
          if (idx < 0) break;
          n = parentOf.get(n);
        }
        if (idx < 0) { if (out.indexOf(node) < 0) out.push(node); break; }
      }
    }
    return out;
  };

  /* ---- hydration: attach non-enumerable accessors to raw nodes --------- */
  const contains = (root, target) => {
    if (root === target) return true;
    for (const c of root.children) if (contains(c, target)) return true;
    return false;
  };
  const descendantText = (n) => {
    if (n.tag === TEXT) return n.text;
    let out = "";
    for (const c of n.children) out += descendantText(c);
    return out;
  };
  const defineGetSet = (obj, name, get, set) => {
    const d = { get, enumerable: false, configurable: true };
    if (set) d.set = set;
    Object.defineProperty(obj, name, d);
  };

  const hydrate = (n, parent) => {
    if (!n || typeof n !== "object" || n.__h) return n;
    Object.defineProperty(n, "__h", { value: true, enumerable: false, configurable: false });
    if (parent) Object.defineProperty(n, "__p", { value: parent, writable: true, enumerable: false, configurable: true });
    if (!Array.isArray(n.attrs)) n.attrs = [];
    if (!Array.isArray(n.children)) n.children = [];
    if (typeof n.text !== "string") n.text = String(n.text || "");

    defineGetSet(n, "textContent", () => (n.tag === TEXT ? n.text : descendantText(n)), (v) => {
      const s = v === null || v === undefined ? "" : String(v);
      if (n.tag === TEXT) { n.text = s; return; }
      n.children = s === "" ? [] : [mkNode(TEXT, s)];
    });
    defineGetSet(n, "innerText", () => n.textContent, (v) => { n.textContent = v; });
    defineGetSet(n, "innerHTML", () => n.children.map(serialize).join(""), (v) => {
      n.children = parseFragment(v);
      for (const c of n.children) hydrate(c, n);
    });
    defineGetSet(n, "outerHTML", () => serialize(n));
    defineGetSet(n, "id", () => getAttr(n, "id") || "", (v) => setAttr(n, "id", v));
    defineGetSet(n, "className", () => getAttr(n, "class") || "", (v) => setAttr(n, "class", v));
    defineGetSet(n, "src", () => getAttr(n, "src") || "", (v) => setAttr(n, "src", v));
    defineGetSet(n, "href", () => getAttr(n, "href") || "", (v) => setAttr(n, "href", v));
    defineGetSet(n, "value", () => getAttr(n, "value") || "", (v) => setAttr(n, "value", v));
    defineGetSet(n, "style", () => {
      if (!n.__style) Object.defineProperty(n, "__style", { value: {}, enumerable: false, writable: true, configurable: true });
      return n.__style;
    });
    defineGetSet(n, "parentNode", () => n.__p || null);

    n.getAttribute = (k) => getAttr(n, k);
    n.setAttribute = (k, v) => setAttr(n, k, v);
    n.removeAttribute = (k) => delAttr(n, k);
    n.hasAttribute = (k) => getAttr(n, k) !== null;
    n.appendChild = (c) => {
      if (!c || typeof c !== "object") throw new TypeError("appendChild: not a node");
      if (contains(c, n)) throw new Error("appendChild: cannot append an ancestor");
      if (c.__p && Array.isArray(c.__p.children)) {
        const pi = c.__p.children.indexOf(c);
        if (pi >= 0) c.__p.children.splice(pi, 1);
      }
      hydrate(c, n);
      n.children.push(c);
      return c;
    };
    n.removeChild = (c) => {
      const i = n.children.indexOf(c);
      if (i < 0) throw new Error("removeChild: not a child");
      n.children.splice(i, 1);
      return c;
    };
    n.remove = () => {
      const p = n.__p;
      if (p && Array.isArray(p.children)) {
        const i = p.children.indexOf(n);
        if (i >= 0) p.children.splice(i, 1);
      }
    };
    n.insertBefore = (c, ref) => {
      if (ref === null || ref === undefined) return n.appendChild(c);
      const i = n.children.indexOf(ref);
      if (i < 0) throw new Error("insertBefore: reference node is not a child");
      hydrate(c, n);
      n.children.splice(i, 0, c);
      return c;
    };
    n.contains = (t) => contains(n, t);
    n.querySelector = (s) => queryAll(n, s)[0] || null;
    n.querySelectorAll = (s) => queryAll(n, s);
    n.getElementsByTagName = (t) => queryAll(n, t);
    n.addEventListener = (type, fn) => addLis(n, type, fn);
    n.removeEventListener = (type, fn) => removeLis(n, type, fn);
    n.dispatchEvent = (evt) => fireAt(n, asEvent(evt, n));
    n.cloneNode = (deep) => {
      const copy = JSON.parse(JSON.stringify(n));
      return hydrate(copy, null);
    };

    for (const c of n.children) hydrate(c, n);
    return n;
  };
  globalThis.__aiosHydrate = hydrate;

  /* ---- document -------------------------------------------------------- */
  const dom = () => globalThis.__dom;
  const findTag = (tag) => {
    const t = tag.toLowerCase();
    let found = null;
    const walk = (n) => {
      if (found) return;
      if (n.tag === t) { found = n; return; }
      for (const c of n.children) walk(c);
    };
    walk(dom());
    return found;
  };
  const findAll = (pred) => {
    const out = [];
    const walk = (n) => { if (pred(n)) out.push(n); for (const c of n.children) walk(c); };
    walk(dom());
    return out;
  };

  const location = {
    get href() { return meta().url; },
    set href(v) { meta().nav = String(v); },
    assign(v) { meta().nav = String(v); },
    replace(v) { meta().nav = String(v); },
    reload() { note("[js] location.reload() is not supported"); },
    toString() { return meta().url; },
    get protocol() { return meta().url.startsWith("https") ? "https:" : "http:"; },
    get host() { return meta().host || ""; },
    get hostname() { return (meta().host || "").split(":")[0]; },
    get port() { const h = meta().host || ""; return h.includes(":") ? h.split(":")[1] : ""; },
    get pathname() { const u = meta().url; const i = u.indexOf("/", u.indexOf("//") + 2); return i < 0 ? "/" : u.slice(i).split("?")[0].split("#")[0]; },
    get search() { const u = meta().url; const i = u.indexOf("?"); return i < 0 ? "" : u.slice(i).split("#")[0]; },
    get hash() { const u = meta().url; const i = u.indexOf("#"); return i < 0 ? "" : u.slice(i); },
    get origin() { const u = meta().url; const i = u.indexOf("/", u.indexOf("//") + 2); return i < 0 ? u : u.slice(0, i); },
  };

  const mkStorage = (bag) => ({
    getItem(k) { k = String(k); return Object.prototype.hasOwnProperty.call(bag, k) ? bag[k] : null; },
    setItem(k, v) { bag[String(k)] = String(v); },
    removeItem(k) { delete bag[String(k)]; },
    clear() { for (const k of Object.keys(bag)) delete bag[k]; },
    key(i) { return Object.keys(bag)[i] || null; },
    get length() { return Object.keys(bag).length; },
  });

  const doc = {
    get readyState() { return meta().readyState || "loading"; },
    get URL() { return meta().url; },
    get documentURI() { return meta().url; },
    get title() { return meta().title; },
    set title(v) { meta().title = String(v); },
    get referrer() { return ""; },
    get cookie() { return meta().cookie || ""; },
    set cookie(v) { meta().cookie = String(v); },
    get hidden() { return false; },
    get visibilityState() { return "visible"; },
    get documentElement() { return hydrate(findTag("html"), null); },
    get head() { return hydrate(findTag("head"), null); },
    get body() { return hydrate(findTag("body"), null); },
    get scripts() { return doc.querySelectorAll("script"); },
    get images() { return doc.querySelectorAll("img"); },
    get forms() { return doc.querySelectorAll("form"); },
    get links() { return doc.querySelectorAll("a[href]"); },
    getElementById(id) {
      const hit = findAll((n) => n.tag !== TEXT && getAttr(n, "id") === String(id));
      return hit.length ? hydrate(hit[0], null) : null;
    },
    getElementsByTagName(t) { return findAll((n) => n.tag === String(t).toLowerCase()).map((n) => hydrate(n, null)); },
    getElementsByClassName(c) {
      const cls = String(c).split(/\s+/).filter(Boolean);
      return findAll((n) => {
        const has = (getAttr(n, "class") || "").split(/\s+/);
        return cls.every((x) => has.includes(x));
      }).map((n) => hydrate(n, null));
    },
    querySelector(s) { const r = queryAll(dom(), s); return r.length ? hydrate(r[0], null) : null; },
    querySelectorAll(s) { return queryAll(dom(), s).map((n) => hydrate(n, null)); },
    createElement(t) { return hydrate(mkNode(String(t).toLowerCase()), null); },
    createTextNode(t) { return hydrate(mkNode(TEXT, String(t)), null); },
    createDocumentFragment() { return hydrate(mkNode("#fragment"), null); },
    write(...parts) { note("[js] document.write is not supported: " + parts.map(fmtArg).join("")); },
    writeln(...parts) { doc.write(...parts); },
    open() { return doc; },
    close() {},
    addEventListener(type, fn) { addLis(doc, type, fn); },
    removeEventListener(type, fn) { removeLis(doc, type, fn); },
    dispatchEvent(evt) { return fireAt(doc, asEvent(evt, doc)); },
    get location() { return location; },
    get defaultView() { return globalThis; },
  };
  globalThis.document = doc;

  /* ---- window / globals ------------------------------------------------ */
  globalThis.location = location;
  globalThis.window = globalThis;
  globalThis.addEventListener = (type, fn) => addLis(globalThis, type, fn);
  globalThis.removeEventListener = (type, fn) => removeLis(globalThis, type, fn);
  globalThis.dispatchEvent = (evt) => fireAt(globalThis, asEvent(evt, globalThis));
  globalThis.self = globalThis;
  globalThis.top = globalThis;
  globalThis.parent = globalThis;
  globalThis.navigator = { userAgent: meta().ua || "AIOS-Browser/0.1", language: "en", onLine: true, platform: "AIOS" };
  globalThis.localStorage = mkStorage(meta().local_storage || (meta().local_storage = {}));
  globalThis.sessionStorage = mkStorage(meta().session_storage || (meta().session_storage = {}));
  globalThis.alert = (m) => note("[js] alert: " + fmtArg(m));
  globalThis.confirm = () => false;
  globalThis.prompt = () => null;
  globalThis.scroll = () => {};
  globalThis.scrollBy = () => {};
  globalThis.scrollTo = () => {};
  globalThis.getSelection = () => ({ toString: () => "", removeAllRanges: () => {}, addRange: () => {} });
  globalThis.requestAnimationFrame = (fn) => { try { fn(0); } catch (e) { note("[js] rAF error: " + errStr(e)); } return 1; };
  globalThis.cancelAnimationFrame = () => {};
  globalThis.matchMedia = () => ({ matches: false, addListener: () => {}, removeListener: () => {}, addEventListener: () => {}, removeEventListener: () => {} });
  /* ---- URL / URLSearchParams shim -------------------------------------- */
  const resolveHref = (raw, base) => {
    let u = String(raw == null ? "" : raw);
    if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(u)) return u;
    const b = String(base || meta().url || "");
    if (u.startsWith("//")) {
      const m = b.match(/^([a-zA-Z][\w+.-]*:)/);
      return (m ? m[1] : "http:") + u;
    }
    if (u.startsWith("/")) {
      const m = b.match(/^([a-zA-Z][\w+.-]*:\/\/[^/?#]*)/);
      return (m ? m[1] : "") + u;
    }
    const cut = b.replace(/[?#].*$/, "");
    const slash = cut.lastIndexOf("/");
    const dir = slash >= 0 ? cut.slice(0, slash + 1) : cut + "/";
    if (u.startsWith("./")) u = u.slice(2);
    return dir + u;
  };
  const urlParts = (u) => {
    const m = u.match(/^([^:]+:)?(?:\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/) || [];
    return { protocol: m[1] || "", host: m[2] || "", pathname: m[3] || "/", search: m[4] || "", hash: m[5] || "" };
  };
  globalThis.URL = class URL {
    constructor(url, base) {
      const href = resolveHref(url, base);
      this.href = href;
      const p = urlParts(href);
      this.protocol = p.protocol;
      this.host = p.host;
      this.hostname = p.host.split(":")[0];
      this.port = p.host.includes(":") ? p.host.split(":")[1] : "";
      this.pathname = p.pathname || "/";
      this.search = p.search;
      this.hash = p.hash;
      this.origin = p.protocol + (p.host ? "//" + p.host : "");
    }
    toString() { return this.href; }
    get searchParams() { return new globalThis.URLSearchParams(this.search.replace(/^\?/, "")); }
  };
  globalThis.URLSearchParams = class URLSearchParams {
    constructor(init) {
      this._map = [];
      if (typeof init === "string") {
        for (const pair of init.replace(/^\?/, "").split("&")) {
          if (!pair) continue;
          const i = pair.indexOf("=");
          const k = i < 0 ? pair : pair.slice(0, i);
          const v = i < 0 ? "" : pair.slice(i + 1);
          try { this._map.push([decodeURIComponent(k), decodeURIComponent(v.replace(/\+/g, " "))]); }
          catch (_) { this._map.push([k, v]); }
        }
      }
    }
    get(k) { for (const p of this._map) if (p[0] === String(k)) return p[1]; return null; }
    has(k) { return this.get(k) !== null; }
    getAll(k) { return this._map.filter((p) => p[0] === String(k)).map((p) => p[1]); }
    set(k, v) {
      for (const p of this._map) if (p[0] === String(k)) { p[1] = String(v); return; }
      this._map.push([String(k), String(v)]);
    }
    append(k, v) { this._map.push([String(k), String(v)]); }
    delete(k) { this._map = this._map.filter((p) => p[0] !== String(k)); }
    toString() { return this._map.map(([a, b]) => encodeURIComponent(a) + "=" + encodeURIComponent(b)).join("&"); }
  };

  /* ---- network bridge: cooperative two-phase fetch/XMLHttpRequest ------- *
   * The engine has no host functions, so a request is parked in __net and
   * the Rust side drains the queue between script phases, fetches through
   * its NetworkClient and calls __aiosNetResolve(id, responseJson). */
  globalThis.__net = { seq: 1, pending: {}, requests: [] };
  const netQueue = (id, entry) => {
    globalThis.__net.pending[id] = entry;
  };
  globalThis.__aiosTakeRequests = () => {
    const r = globalThis.__net.requests;
    globalThis.__net.requests = [];
    return r;
  };
  const mkHeaders = (map) => ({
    get(name) {
      const k = String(name).toLowerCase();
      return Object.prototype.hasOwnProperty.call(map, k) ? map[k] : null;
    },
    has(name) { return this.get(name) !== null; },
    forEach(fn) { for (const k of Object.keys(map)) fn(map[k], k, this); },
  });
  const mkResponse = (res) => {
    const text = res.body == null ? "" : String(res.body);
    const headers = mkHeaders(res.headers || {});
    const response = {
      ok: res.status >= 200 && res.status < 300,
      status: res.status,
      statusText: res.status_text || "",
      url: res.url || "",
      redirected: false,
      type: "basic",
      headers,
      text: () => Promise.resolve(text),
      json: () => {
        try { return Promise.resolve(JSON.parse(text)); }
        catch (e) { return Promise.reject(e); }
      },
      arrayBuffer: () => Promise.resolve(new ArrayBuffer(0)),
      blob: () => Promise.resolve(null),
      clone: () => mkResponse(res),
    };
    return response;
  };
  globalThis.__aiosNetResolve = (id, res) => {
    const entry = globalThis.__net.pending[id];
    if (!entry) return;
    delete globalThis.__net.pending[id];
    try {
      if (res.error) entry.reject(new TypeError("Failed to fetch: " + res.error));
      else if (entry.xhr) entry.resolve(res);
      else entry.resolve(mkResponse(res));
    } catch (e) { note("[js] fetch resolve error: " + errStr(e)); }
  };
  const normalizeUrl = (input) => {
    if (typeof input === "string") return input;
    if (input && typeof input.url === "string") return input.url;
    return String(input);
  };
  const absoluteUrl = (u) => {
    try { return new URL(u, meta().url).toString(); } catch (_) { return String(u); }
  };
  globalThis.fetch = (input, init) => {
    init = init || {};
    const method = String(init.method || "GET").toUpperCase();
    const body = init.body != null ? String(init.body) : null;
    const url = absoluteUrl(normalizeUrl(input));
    return new Promise((resolve, reject) => {
      const id = String(globalThis.__net.seq++);
      netQueue(id, { resolve, reject });
      globalThis.__net.requests.push({ id, url, method, body, kind: "fetch" });
    });
  };
  globalThis.Request = class Request {
    constructor(input, init) {
      init = init || {};
      this.url = absoluteUrl(normalizeUrl(input));
      this.method = String(init.method || "GET").toUpperCase();
      this.body = init.body != null ? init.body : null;
      this.headers = mkHeaders(init.headers || {});
    }
  };
  globalThis.Headers = class Headers {
    constructor(init) {
      this._map = {};
      if (init) for (const k of Object.keys(init)) this._map[String(k).toLowerCase()] = String(init[k]);
    }
    get(n) { const k = String(n).toLowerCase(); return Object.prototype.hasOwnProperty.call(this._map, k) ? this._map[k] : null; }
    set(n, v) { this._map[String(n).toLowerCase()] = String(v); }
    has(n) { return this.get(n) !== null; }
    forEach(fn) { for (const k of Object.keys(this._map)) fn(this._map[k], k, this); }
  };
  globalThis.XMLHttpRequest = class XMLHttpRequest {
    constructor() {
      this.readyState = 0;
      this.status = 0;
      this.statusText = "";
      this.responseText = "";
      this.response = "";
      this.responseType = "";
      this.onreadystatechange = null;
      this.onload = null;
      this.onerror = null;
      this.onloadend = null;
      this._headers = {};
      this._listeners = {};
      this._method = "GET";
      this._url = "";
    }
    open(method, url) {
      this._method = String(method || "GET").toUpperCase();
      this._url = absoluteUrl(String(url));
      this.readyState = 1;
      this._fire("readystatechange");
    }
    setRequestHeader(k, v) { this._headers[String(k)] = String(v); }
    addEventListener(type, fn) { (this._listeners[type] = this._listeners[type] || []).push(fn); }
    removeEventListener(type, fn) {
      const list = this._listeners[type];
      if (list) { const i = list.indexOf(fn); if (i >= 0) list.splice(i, 1); }
    }
    _fire(type) {
      const ev = { type, target: this, currentTarget: this };
      const prop = this["on" + type];
      if (typeof prop === "function") { try { prop.call(this, ev); } catch (e) { note("[js] xhr " + type + " error: " + errStr(e)); } }
      const list = this._listeners[type];
      if (list) for (const fn of list.slice()) { try { fn.call(this, ev); } catch (e) { note("[js] xhr " + type + " error: " + errStr(e)); } }
    }
    send(body) {
      this.readyState = 2;
      const id = String(globalThis.__net.seq++);
      netQueue(id, {
        xhr: true,
        resolve: (res) => this._done(res),
        reject: () => this._fail(),
      });
      globalThis.__net.requests.push({
        id, url: this._url, method: this._method,
        body: body != null ? String(body) : null, kind: "xhr",
      });
    }
    _done(res) {
      this.status = res.status || 0;
      this.statusText = res.status_text || "";
      this._responseHeaders = res.headers || {};
      this.responseText = res.body == null ? "" : String(res.body);
      this.response = this.responseText;
      this.readyState = 4;
      this._fire("readystatechange");
      this._fire("load");
      this._fire("loadend");
    }
    _fail() {
      this.status = 0;
      this.readyState = 4;
      this._fire("readystatechange");
      this._fire("error");
      this._fire("loadend");
    }
    abort() {}
    getAllResponseHeaders() {
      const map = (this._responseHeaders || {});
      return Object.keys(map).map((k) => k + ": " + map[k]).join("\r\n");
    }
    getResponseHeader() { return null; }
  };

  /* ---- lifecycle events + dynamic <script> discovery -------------------- */
  globalThis.__aiosFire = (type) => {
    const m = meta();
    if (type === "DOMContentLoaded") {
      m.readyState = "interactive";
      fireAt(doc, mkEvent("readystatechange", doc));
      fireAt(doc, mkEvent("DOMContentLoaded", doc));
    } else if (type === "load") {
      m.readyState = "complete";
      fireAt(doc, mkEvent("readystatechange", doc));
      fireAt(globalThis, mkEvent("load", doc));
      fireAt(doc, mkEvent("load", doc));
    }
    return true;
  };

  const markScript = (n) => {
    if (!n.__ran) Object.defineProperty(n, "__ran", { value: true, enumerable: false, configurable: true });
  };
  const isJsType = (raw) => {
    const kind = String(raw || "").toLowerCase().split(";")[0].trim();
    return kind === "" || kind === "text/javascript" || kind === "application/javascript"
      || kind === "text/ecmascript" || kind === "application/ecmascript";
  };
  const scriptCode = (n) => {
    let out = "";
    for (const c of n.children) if (c.tag === TEXT) out += c.text;
    return out;
  };
  /* Mark the parser-inserted batch (executed by the Rust side itself) so
   * later collection only yields page-created elements. */
  globalThis.__aiosMarkScripts = () => {
    const walk = (n) => { if (n.tag === "script") markScript(n); for (const c of n.children) walk(c); };
    walk(dom());
  };
  /* New <script> elements appended by page code: marked here, returned once. */
  globalThis.__aiosCollectScripts = () => {
    const out = [];
    const walk = (n) => {
      if (n.tag === "script" && !n.__ran) {
        markScript(n);
        const type = getAttr(n, "type") || "";
        const isModule = String(type).toLowerCase() === "module";
        if (!isModule && !isJsType(type)) return;
        const src = getAttr(n, "src");
        if (src) { out.push({ src: String(src), code: null, module: isModule }); return; }
        const code = scriptCode(n);
        if (code.trim()) out.push({ src: null, code, module: isModule });
      }
      for (const c of n.children) walk(c);
    };
    walk(dom());
    return out;
  };

  /* ---- timers: queued, flushed once after load, capped ------------------ */
  globalThis.__timers = [];
  let nextTimerId = 1;
  globalThis.setTimeout = (fn, ms, ...rest) => {
    const id = nextTimerId++;
    if (typeof fn === "function") globalThis.__timers.push({ id, fn, ms: Number(ms) || 0, args: rest });
    return id;
  };
  globalThis.setInterval = (fn, ms, ...rest) => {
    const id = nextTimerId++;
    if (typeof fn === "function") globalThis.__timers.push({ id, fn, ms: Number(ms) || 0, args: rest, rep: true });
    return id;
  };
  const dropTimer = (id) => {
    const i = globalThis.__timers.findIndex((t) => t.id === id);
    if (i >= 0) globalThis.__timers.splice(i, 1);
  };
  globalThis.clearTimeout = dropTimer;
  globalThis.clearInterval = dropTimer;
  globalThis.queueMicrotask = (fn) => {
    try { fn(); } catch (e) { note("[js] microtask error: " + errStr(e)); }
  };
  globalThis.__aiosFlushTimers = (max) => {
    const done = new Set();
    let ran = 0;
    while (globalThis.__timers.length && ran < max) {
      globalThis.__timers.sort((a, b) => a.ms - b.ms);
      const t = globalThis.__timers.shift();
      if (!t || done.has(t.id)) continue;
      done.add(t.id);
      ran++;
      try { t.fn.apply(null, t.args || []); } catch (e) { note("[js] timer error: " + errStr(e)); }
    }
    if (globalThis.__timers.length) {
      note("[js] timer queue limit reached, " + globalThis.__timers.length + " callbacks dropped");
    }
    return ran;
  };

  /* ---- boot: the Rust side injects __dom/__meta before evaluating this file. */
  if (globalThis.__dom) hydrate(globalThis.__dom, null);
})();

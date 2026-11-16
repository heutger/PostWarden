// SPDX-License-Identifier: AGPL-3.0-only
//
// PostWarden initialization script (runs in the main frame before Outlook's own scripts).
//
// Reads ONLY what is visible: the inbox unread counter, banner texts (checked against
// configured patterns – only the matching *pattern* is reported, never the text), error
// screens and the page title. It never clicks, fetches, moves or reads mail content.
// All selectors and patterns come from config.json via `pw_get_config`.
(function () {
  'use strict';

  if (window.top !== window) return; // main frame only
  if (window.opener) return; // pop-outs (compose window etc.) are not monitored
  if (window.__pw) return;
  if (location.protocol !== 'https:') return;
  // Outlook hosts only (the same list as capabilities/remote.json). Sign-in and IdP
  // pages stay untouched: no Notification patch, no listeners.
  if (!/^(outlook\.office\.com|outlook\.office365\.com|outlook\.cloud\.microsoft|outlook\.live\.com)$/i.test(location.hostname)) return;

  // Captured before page scripts run.
  var tauri = window.__TAURI_INTERNALS__;
  function invoke(cmd, args) {
    var t = tauri || window.__TAURI_INTERNALS__;
    if (!t || typeof t.invoke !== 'function') {
      return Promise.reject(new Error('PostWarden IPC unavailable'));
    }
    return t.invoke(cmd, args || {});
  }

  var cfg = null;
  var armed = false;
  var configured = false;
  var started = false;
  var lastKey = null;
  var lastSent = 0;
  var HEARTBEAT_MS = 60000;

  // Mail areas (message list, reading pane, compose). Fixed – not configurable – and
  // never read for banners or error texts.
  var MAIL_AREAS = [
    "[role='listbox']",
    "[role='option']",
    "[role='document']",
    "[contenteditable='true']",
    "[contenteditable='']",
    'textarea',
    "[aria-label*='Nachrichtentext']",
    "[aria-label*='Message body']",
    "[aria-label*='Nachrichtenliste']",
    "[aria-label*='Message list']",
    "[aria-label*='Lesebereich']",
    "[aria-label*='Reading pane']",
    "[aria-label*='Verfassen']",
    "[aria-label*='Compose']",
  ].join(', ');

  // ---------------------------------------------------------------- notification bridge
  // Outlook's own web notifications are forwarded to macOS. Title/body are only passed
  // on if the user enabled `notification_show_content`.
  function clip(s) {
    return typeof s === 'string' ? s.slice(0, 200) : null;
  }
  function forward(title, options) {
    if (!armed) return; // pop-outs share this script but never forward
    var show = !!(cfg && cfg.showContent);
    invoke('pw_notify', {
      kind: 'outlook',
      title: show ? clip(title) : null,
      body: show ? clip(options && options.body) : null,
    }).catch(function () {});
  }

  var Base = typeof EventTarget === 'function' ? EventTarget : Object;
  class PWNotification extends Base {
    constructor(title, options) {
      super();
      options = options || {};
      this.title = String(title || '');
      this.body = String(options.body || '');
      this.tag = String(options.tag || '');
      this.icon = String(options.icon || '');
      this.data = options.data === undefined ? null : options.data;
      this.onclick = null;
      this.onshow = null;
      this.onclose = null;
      this.onerror = null;
      forward(title, options);
      var self = this;
      setTimeout(function () {
        self._fire('show');
      }, 0);
    }
    _fire(type) {
      var ev = new Event(type);
      if (this.dispatchEvent) this.dispatchEvent(ev);
      var handler = this['on' + type];
      if (typeof handler === 'function') handler.call(this, ev);
    }
    close() {
      this._fire('close');
    }
    static get permission() {
      return 'granted';
    }
    static get maxActions() {
      return 0;
    }
    static requestPermission(callback) {
      if (typeof callback === 'function') callback('granted');
      return Promise.resolve('granted');
    }
  }
  try {
    Object.defineProperty(window, 'Notification', {
      value: PWNotification,
      writable: true,
      configurable: true,
    });
  } catch (e) {
    window.Notification = PWNotification;
  }
  try {
    if (window.ServiceWorkerRegistration && ServiceWorkerRegistration.prototype) {
      ServiceWorkerRegistration.prototype.showNotification = function (title, options) {
        forward(title, options);
        return Promise.resolve();
      };
    }
    if (navigator.permissions && typeof navigator.permissions.query === 'function') {
      var query = navigator.permissions.query.bind(navigator.permissions);
      navigator.permissions.query = function (desc) {
        if (desc && desc.name === 'notifications') {
          return Promise.resolve({
            name: 'notifications',
            state: 'granted',
            onchange: null,
            addEventListener: function () {},
            removeEventListener: function () {},
          });
        }
        return query(desc);
      };
    }
  } catch (e) {
    /* ignore */
  }

  // ---------------------------------------------------------------- config
  function validSelectors(list) {
    var probe = document.createDocumentFragment();
    var ok = (list || []).filter(function (s) {
      if (typeof s !== 'string' || !s.trim()) return false;
      try {
        probe.querySelector(s);
        return true;
      } catch (e) {
        return false;
      }
    });
    return ok.length ? ok.join(', ') : null;
  }
  function joinSel(a, b) {
    return [a, b].filter(Boolean).join(', ') || null;
  }
  function regex(p) {
    try {
      return typeof p === 'string' && p ? new RegExp(p, 'i') : null;
    } catch (e) {
      return null;
    }
  }
  function regexList(list) {
    return (list || [])
      .map(function (p) {
        return { src: String(p), re: regex(p) };
      })
      .filter(function (x) {
        return x.re;
      });
  }
  function compile(c) {
    var u = c.unread || {};
    var b = c.auth_banner || {};
    return {
      appHosts: (c.app_hosts || []).map(lower),
      inboxNames: (u.inbox_names || []).map(lower).filter(Boolean),
      unreadSel: validSelectors(u.selectors),
      countRe: regex(u.count_regex),
      titleRe: regex(u.title_regex),
      bannerSel: validSelectors(b.selectors),
      bannerPatterns: regexList(b.patterns),
      bannerExclude: joinSel(MAIL_AREAS, validSelectors(b.exclude_within)),
      maxText: b.max_text_length || 500,
      errorSel: validSelectors((c.error_page || {}).selectors),
      errorPatterns: regexList((c.error_page || {}).patterns),
      pollMs: Math.max(5, c.poll_seconds || 30) * 1000,
      showContent: !!c.notification_show_content,
    };
  }

  // ---------------------------------------------------------------- helpers
  function lower(s) {
    return String(s || '').toLowerCase();
  }
  function squash(s) {
    return String(s || '').replace(/\s+/g, ' ').trim();
  }
  function textOf(el) {
    return squash(el.innerText || el.textContent || '');
  }
  // Visible on screen. Screen-reader-only live regions (1px, clipped) do not count.
  function isVisible(el) {
    if (!el || !el.isConnected) return false;
    var r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) return false;
    var st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none') return false;
    // Opacity is not inherited: a fully transparent ancestor hides the element too.
    for (var p = el; p && p.nodeType === 1; p = p.parentElement) {
      if (getComputedStyle(p).opacity === '0') return false;
    }
    return true;
  }
  function within(el, sel) {
    return !!(sel && el.closest(sel));
  }
  // Inside a mail area or containing one.
  function touches(el, sel) {
    return !!(sel && (el.closest(sel) || el.querySelector(sel)));
  }
  function isMailView() {
    return /^\/mail(\/|$)/i.test(location.pathname);
  }
  function labelsOf(el) {
    return [el.getAttribute('title'), el.getAttribute('aria-label'), textOf(el)]
      .map(function (s) {
        return squash(s).slice(0, 300);
      })
      .filter(Boolean);
  }
  function firstNumber(m) {
    if (!m) return null;
    for (var i = 1; i < m.length; i++) {
      if (/^\d+$/.test(m[i] || '')) return parseInt(m[i], 10);
    }
    return null;
  }

  // ---------------------------------------------------------------- detection
  function findBanner() {
    if (!cfg.bannerSel || !cfg.bannerPatterns.length) return null;
    var els = document.querySelectorAll(cfg.bannerSel);
    for (var i = 0; i < els.length; i++) {
      var el = els[i];
      if (touches(el, cfg.bannerExclude) || !isVisible(el)) continue;
      var t = lower(textOf(el));
      if (!t || t.length > cfg.maxText) continue;
      for (var j = 0; j < cfg.bannerPatterns.length; j++) {
        if (cfg.bannerPatterns[j].re.test(t)) return cfg.bannerPatterns[j].src;
      }
    }
    return null;
  }

  // Unread count of a folder element: count_regex on title/aria-label/text, otherwise a
  // digits-only child that does not belong to a nested subfolder.
  function countOf(el, labels) {
    if (cfg.countRe) {
      for (var i = 0; i < labels.length; i++) {
        var n = firstNumber(cfg.countRe.exec(labels[i]));
        if (n !== null) return n;
      }
    }
    var nodes = el.querySelectorAll('span, div, b, strong, i');
    for (var k = 0; k < nodes.length; k++) {
      var c = nodes[k];
      if (c.children.length) continue;
      var group = c.closest('[role="group"], [role="treeitem"]');
      if (group && group !== el && el.contains(group)) continue;
      var m = /^(\d{1,6})\+?$/.exec(squash(c.textContent));
      if (m) return parseInt(m[1], 10);
    }
    return 0;
  }

  function findInbox() {
    if (!cfg.unreadSel || !cfg.inboxNames.length) return null;
    var els = document.querySelectorAll(cfg.unreadSel);
    var fallback = null;
    for (var i = 0; i < els.length; i++) {
      var el = els[i];
      var labels = labelsOf(el);
      var label = null;
      for (var j = 0; j < labels.length && !label; j++) {
        var low = lower(labels[j]);
        for (var k = 0; k < cfg.inboxNames.length; k++) {
          if (low.indexOf(cfg.inboxNames[k]) === 0) {
            label = labels[j];
            break;
          }
        }
      }
      if (!label) continue;
      var hit = { el: el, label: label, count: countOf(el, labels), visible: isVisible(el) };
      if (hit.visible) return hit;
      if (!fallback) fallback = hit;
    }
    return fallback;
  }

  // Visible page text outside mail areas, at most `limit` characters.
  function visibleText(skip, limit) {
    if (!document.body) return '';
    var parts = [];
    var len = 0;
    var walker = document.createTreeWalker(
      document.body,
      NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT,
      {
        acceptNode: function (n) {
          if (n.nodeType === 3) return NodeFilter.FILTER_ACCEPT;
          if (/^(SCRIPT|STYLE|NOSCRIPT|TEMPLATE)$/.test(n.tagName)) return NodeFilter.FILTER_REJECT;
          if (skip && n.matches(skip)) return NodeFilter.FILTER_REJECT;
          if (!n.getClientRects().length) return NodeFilter.FILTER_REJECT; // display:none
          if (getComputedStyle(n).visibility === 'hidden') return NodeFilter.FILTER_REJECT;
          return NodeFilter.FILTER_SKIP;
        },
      }
    );
    var node;
    while ((node = walker.nextNode()) && len < limit) {
      var t = squash(node.nodeValue);
      if (t) {
        parts.push(t);
        len += t.length + 1;
      }
    }
    return parts.join(' ').slice(0, limit);
  }

  // Error screens replace the app: they are short and have no folder tree. Checked
  // whenever no live inbox is visible – also when the page has a generic
  // <main>/[role=main] – but never inside mail areas. A long page (calendar, settings)
  // or a visible folder tree is the app itself, whose texts may contain the same words.
  var ERROR_SCREEN_MAX = 1500;
  // Only the message elements (`error_page.selectors`: headings, alerts) are matched:
  // a short calendar or task view may contain the same words in ordinary UI text.
  function findErrorPattern() {
    if (!cfg.errorPatterns.length || !cfg.errorSel) return null;
    var trees = document.querySelectorAll("[role='tree']");
    for (var i = 0; i < trees.length; i++) {
      if (isVisible(trees[i])) return null;
    }
    if (visibleText(cfg.bannerExclude, ERROR_SCREEN_MAX + 1).length > ERROR_SCREEN_MAX) return null;
    var els = document.querySelectorAll(cfg.errorSel);
    for (var k = 0; k < els.length; k++) {
      var el = els[k];
      if (!isVisible(el) || touches(el, cfg.bannerExclude)) continue;
      var text = lower(textOf(el));
      for (var j = 0; j < cfg.errorPatterns.length; j++) {
        if (cfg.errorPatterns[j].re.test(text)) return cfg.errorPatterns[j].src;
      }
    }
    return null;
  }

  // Life sign = a *visible* inbox folder element. The page title and hidden (stale)
  // folder trees only provide the counter, never a life sign; banner and error checks
  // run independently of the counter.
  function compute() {
    var banner = findBanner();
    var inbox = findInbox();
    var alive = !!(inbox && inbox.visible);
    var unread = alive ? inbox.count : null;
    if (!alive && cfg.titleRe) {
      var n = firstNumber(cfg.titleRe.exec(document.title || ''));
      if (n !== null) unread = n;
    }
    var errorPattern = alive ? null : findErrorPattern();
    return {
      unread: unread,
      inbox_found: alive,
      auth_banner: banner,
      error_page: errorPattern !== null,
      offline: navigator.onLine === false,
      mail_view: isMailView(),
    };
  }

  function detect() {
    if (!cfg) return null;
    var state;
    try {
      state = compute();
    } catch (e) {
      state = {
        unread: null,
        inbox_found: false,
        auth_banner: null,
        error_page: false,
        offline: navigator.onLine === false,
        mail_view: isMailView(),
        };
    }
    var key = JSON.stringify(state);
    var now = Date.now();
    if (key !== lastKey || now - lastSent >= HEARTBEAT_MS) {
      lastKey = key;
      lastSent = now;
      invoke('pw_report', { state: state }).catch(function () {
        lastKey = null;
      });
    }
    return state;
  }

  var pending = null;
  function schedule() {
    if (pending) return; // throttle: at most one check per second
    pending = setTimeout(function () {
      pending = null;
      detect();
    }, 1000);
  }

  // ---------------------------------------------------------------- start
  function start() {
    var observer = new MutationObserver(schedule);
    observer.observe(document, {
      childList: true,
      subtree: true,
      characterData: true,
      attributes: true,
      attributeFilter: ['aria-label', 'title', 'role', 'hidden'],
    });
    setInterval(detect, cfg.pollMs);
    ['online', 'offline', 'focus', 'pageshow'].forEach(function (ev) {
      window.addEventListener(ev, schedule);
    });
    document.addEventListener('visibilitychange', schedule);
    schedule();
  }

  // Monitoring starts only once PostWarden's main window arms it (Rust calls arm()/tick()
  // on the main window only). Until then the script makes no IPC call at all: pop-outs
  // share this script but never fetch the config, report, forward or zoom.
  function maybeStart() {
    if (armed && configured && !started) {
      started = true;
      start();
    }
  }
  function arm() {
    armed = true;
    if (!configured) boot();
    maybeStart();
  }

  // Fetches the config. One chain of retries at a time; a later arm()/tick() (Rust calls
  // tick() every poll interval) starts a new chain at most once per minute, so a passing
  // IPC failure cannot leave the monitoring silent until the next reload.
  // Each call has a deadline: an IPC answer that never arrives cannot block later
  // attempts, and a late answer of an abandoned attempt is ignored.
  var booting = false;
  var lastBoot = 0;
  var bootId = 0;
  var CONFIG_TIMEOUT_MS = 10000;
  function withTimeout(promise, ms) {
    return new Promise(function (resolve, reject) {
      var timer = setTimeout(function () {
        reject(new Error('timeout'));
      }, ms);
      promise.then(
        function (v) {
          clearTimeout(timer);
          resolve(v);
        },
        function (e) {
          clearTimeout(timer);
          reject(e);
        }
      );
    });
  }
  function boot() {
    if (booting || configured || Date.now() - lastBoot < 60000) return;
    booting = true;
    lastBoot = Date.now();
    var id = ++bootId;
    (function attempt(n) {
      withTimeout(invoke('pw_get_config'), CONFIG_TIMEOUT_MS)
        .then(function (c) {
          if (id !== bootId || configured) return;
          booting = false;
          cfg = compile(c);
          if (cfg.appHosts.length && cfg.appHosts.indexOf(lower(location.hostname)) < 0) return;
          configured = true;
          maybeStart();
        })
        .catch(function () {
          if (id !== bootId) return;
          if (n < 5) {
            setTimeout(function () {
              attempt(n + 1);
            }, 500 * Math.pow(2, n));
          } else {
            booting = false;
          }
        });
    })(0);
  }

  // ⌘+ on keyboards where "+" is its own key (the menu covers ⌘=, ⌘-, ⌘0).
  window.addEventListener(
    'keydown',
    function (e) {
      if (armed && e.metaKey && !e.ctrlKey && !e.altKey && e.key === '+') {
        e.preventDefault();
        invoke('pw_zoom', { direction: 'in' }).catch(function () {});
      }
    },
    true
  );

  Object.defineProperty(window, '__pw', {
    value: Object.freeze({
      arm: arm,
      tick: function () {
        arm();
        return detect();
      },
    }),
    writable: false,
    configurable: false,
    enumerable: false,
  });

})();

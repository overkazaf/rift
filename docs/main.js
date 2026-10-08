// Rift site: hero terminal demo, copy buttons, provider tabs. No dependencies.
(function () {
  "use strict";

  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // ── Hero demo ──
  // The HTML already contains the final state, so no-JS and reduced-motion
  // visitors see a complete, static terminal. With motion allowed we hide the
  // steps and replay them in document order.
  var demo = document.getElementById("demo");
  var replay = document.getElementById("replay");
  var timers = [];

  function later(fn, ms) { timers.push(setTimeout(fn, ms)); }
  function clearTimers() { timers.forEach(clearTimeout); timers = []; }

  function run() {
    if (!demo) return;
    clearTimers();
    var nodes = Array.prototype.slice.call(demo.querySelectorAll("[data-step],[data-type]"));
    nodes.forEach(function (el) {
      el.classList.remove("on", "typing");
      if (el.hasAttribute("data-type")) {
        if (el._full == null) el._full = el.textContent;
        el.textContent = "";
      }
    });
    demo.classList.add("anim");
    if (replay) replay.hidden = true;

    var t = 500;
    nodes.forEach(function (el) {
      if (el.hasAttribute("data-type")) {
        var text = el._full;
        var speed = el.hasAttribute("data-fast") ? 14 : 45;
        later(function () { el.classList.add("typing"); }, t);
        for (var i = 1; i <= text.length; i++) {
          (function (n) {
            later(function () { el.textContent = text.slice(0, n); }, t + n * speed);
          })(i);
        }
        t += text.length * speed + 380;
        later(function () { el.classList.remove("typing"); }, t - 60);
      } else {
        later(function () { el.classList.add("on"); }, t);
        t += el.classList.contains("out") ? 520 : 300;
      }
    });
    later(function () { if (replay) replay.hidden = false; }, t + 400);
  }

  if (demo && !reduce) {
    run();
    if (replay) replay.addEventListener("click", run);
  }

  // ── Copy buttons ──
  Array.prototype.forEach.call(document.querySelectorAll("[data-copy]"), function (btn) {
    btn.addEventListener("click", function () {
      var pre = document.getElementById(btn.getAttribute("data-copy"));
      if (!pre || !navigator.clipboard) return;
      navigator.clipboard.writeText(pre.innerText.trim()).then(function () {
        btn.textContent = "Copied";
        setTimeout(function () { btn.textContent = "Copy"; }, 1400);
      });
    });
  });

  // ── Provider tabs (WAI-ARIA tabs pattern) ──
  var tablist = document.querySelector('[role="tablist"]');
  if (tablist) {
    var tabs = Array.prototype.slice.call(tablist.querySelectorAll('[role="tab"]'));
    var select = function (tab) {
      tabs.forEach(function (t) {
        var on = t === tab;
        t.setAttribute("aria-selected", on ? "true" : "false");
        t.tabIndex = on ? 0 : -1;
        document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
      });
      tab.focus();
    };
    tabs.forEach(function (tab, i) {
      tab.addEventListener("click", function () { select(tab); });
      tab.addEventListener("keydown", function (e) {
        var j = null;
        if (e.key === "ArrowRight") j = (i + 1) % tabs.length;
        else if (e.key === "ArrowLeft") j = (i - 1 + tabs.length) % tabs.length;
        else if (e.key === "Home") j = 0;
        else if (e.key === "End") j = tabs.length - 1;
        if (j !== null) { e.preventDefault(); select(tabs[j]); }
      });
    });
  }
})();

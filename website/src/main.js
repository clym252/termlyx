const ua = navigator.userAgent;
const platform = navigator.userAgentData?.platform || ua;
// Phones and tablets have no build yet (iPadOS reports "Macintosh" but has touch); they keep the jump to #download.
const mobile = /iPhone|iPad|iPod|Android/i.test(ua) || (/Mac/i.test(platform) && navigator.maxTouchPoints > 1);
const os = mobile ? null : /Windows/i.test(platform) ? "win" : /Mac/i.test(platform) ? "mac" : null;

// (a) Mac and Windows visitors: the nav and hero buttons download the right installer directly.
if (os) {
  const dl = document.getElementById(os === "win" ? "dl-win" : "dl-mac");
  const name = os === "win" ? "Windows" : "macOS";
  for (const a of document.querySelectorAll("[data-detect]")) {
    a.href = dl.href;
    a.setAttribute("aria-label", `Download Termlyx for ${name}`);
    const label = a.querySelector("[data-label]");
    if (label) label.textContent = `Download for ${name}`;
    const icon = a.querySelector(os === "win" ? "[data-icon-win]" : "[data-icon-mac]");
    if (icon) icon.hidden = false;
  }
  const note = document.querySelector("[data-note]");
  if (note) note.textContent = os === "win" ? "Windows installer · SmartScreen asks once" : "Universal · signed and notarized";
}

// Windows visitors also get the Windows download first and primary-styled in the download section.
if (os === "win") {
  const P = "bg-accent text-onaccent font-semibold";
  const S = "border border-rule hover:border-accent/50 font-medium";
  const mac = document.getElementById("dl-mac"), win = document.getElementById("dl-win");
  mac.classList.remove(...P.split(" ")); mac.classList.add(...S.split(" "));
  win.classList.remove(...S.split(" ")); win.classList.add(...P.split(" "));
  win.parentElement.prepend(win);
}

// (d) Self-host box: OS tabs and copy button.
const cmd = document.querySelector("[data-cmd]");
if (cmd) {
  const glyph = document.querySelector("[data-glyph]");
  const tabs = [...document.querySelectorAll("[data-os]")];
  for (const t of tabs) t.addEventListener("click", () => {
    const win = t.dataset.os === "win";
    cmd.textContent = win ? cmd.dataset.win : cmd.dataset.unix;
    glyph.textContent = win ? ">" : "$";
    for (const o of tabs) {
      const on = o === t;
      o.setAttribute("aria-pressed", on);
      o.classList.toggle("text-ink", on); o.classList.toggle("text-muted", !on);
      o.classList.toggle("border-accent", on); o.classList.toggle("border-transparent", !on);
    }
  });
  const copy = document.querySelector("[data-copy]");
  const label = copy.querySelector("[data-copy-label]");
  copy.addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(cmd.textContent); label.textContent = "Copied"; }
    catch { label.textContent = "Copy failed"; }
    setTimeout(() => { label.textContent = "Copy"; }, 1500);
  });
}

const videos = [...document.querySelectorAll("video")];

// (b) Reduced motion: no autoplay but controls to play by hand, and CSS stops the rail dot.
if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
  document.documentElement.classList.add("reduced");
  for (const v of videos) { v.removeAttribute("autoplay"); v.pause(); v.controls = true; }
} else {
  // (WCAG 2.2.2) Click or Enter/Space pauses and resumes a clip; a clip the visitor paused stays paused.
  const toggle = (v) => { if (v.paused) { delete v.dataset.userPaused; v.play().catch(() => {}); } else { v.dataset.userPaused = "1"; v.pause(); } };
  for (const v of videos) {
    v.addEventListener("click", () => toggle(v));
    v.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(v); } });
  }
  // (c) Clips play only while on screen: lazy ones from 30% visible, the hero (often cut by the fold) from its first pixel.
  const watch = (list, threshold) => {
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) e.isIntersecting ? e.target.dataset.userPaused || e.target.play().catch(() => {}) : e.target.pause();
    }, { threshold });
    for (const v of list) io.observe(v);
  };
  watch(videos.filter((v) => !v.hasAttribute("data-hero")), 0.3);
  watch(videos.filter((v) => v.hasAttribute("data-hero")), 0);
}

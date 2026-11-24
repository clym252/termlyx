const ua = navigator.userAgentData?.platform || navigator.userAgent;
const P = "bg-accent text-onaccent font-semibold";
const S = "border border-rule hover:border-accent/50 font-medium";

// (a) Windows visitors get the Windows download first and primary-styled.
if (/Windows/i.test(ua)) {
  for (const box of document.querySelectorAll("[data-downloads]")) {
    const mac = box.querySelector('a[id^="dl-mac"]');
    const win = box.querySelector('a[id^="dl-win"]');
    if (!mac || !win) continue;
    const item = (a) => { while (a.parentElement !== box) a = a.parentElement; return a; };
    mac.classList.remove(...P.split(" ")); mac.classList.add(...S.split(" "));
    win.classList.remove(...S.split(" ")); win.classList.add(...P.split(" "));
    box.prepend(item(win));
    if (mac.id === "dl-mac") { mac.lastChild.textContent = "macOS"; win.lastChild.textContent = "Download for Windows"; }
  }
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

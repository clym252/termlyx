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
  }
}

const videos = [...document.querySelectorAll("video")];

// (b) Reduced motion: no autoplay but controls to play by hand, and CSS stops the rail dot.
if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
  document.documentElement.classList.add("reduced");
  for (const v of videos) { v.removeAttribute("autoplay"); v.pause(); v.controls = true; }
} else {
  // (c) Clips play only while on screen: lazy ones from 30% visible, the hero (often cut by the fold) from its first pixel.
  const watch = (list, threshold) => {
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) e.isIntersecting ? e.target.play().catch(() => {}) : e.target.pause();
    }, { threshold });
    for (const v of list) io.observe(v);
  };
  watch(videos.filter((v) => !v.hasAttribute("data-hero")), 0.3);
  watch(videos.filter((v) => v.hasAttribute("data-hero")), 0);
}

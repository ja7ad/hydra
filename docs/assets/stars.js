// GitHub star count in the nav, with a small burst of sparks on hover.
(() => {
  const link = document.querySelector(".nav-gh");
  if (!link) return;
  const badge = link.querySelector(".gh-stars");
  const count = link.querySelector("[data-gh-stars]");
  const star = link.querySelector(".gh-star");
  const CACHE_KEY = "hydra-gh-stars";
  const CACHE_MS = 60 * 60 * 1000;

  const format = (n) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, "")}k` : String(n));
  function show(n) {
    if (!badge || !count) return;
    count.textContent = format(n);
    badge.hidden = false;
    link.setAttribute("aria-label", `Hydra on GitHub, ${n} stars`);
  }

  let cached = null;
  try { cached = JSON.parse(sessionStorage.getItem(CACHE_KEY)); } catch (_) { /* storage unavailable */ }
  if (cached && Date.now() - cached.t < CACHE_MS) {
    show(cached.n);
  } else {
    fetch("https://api.github.com/repos/ja7ad/hydra")
      .then((res) => (res.ok ? res.json() : null))
      .then((repo) => {
        if (!repo || !Number.isFinite(repo.stargazers_count)) return;
        show(repo.stargazers_count);
        try { sessionStorage.setItem(CACHE_KEY, JSON.stringify({ n: repo.stargazers_count, t: Date.now() })); } catch (_) { /* storage unavailable */ }
      })
      .catch(() => {});
  }

  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches || !Element.prototype.animate) return;
  const COLORS = ["#2dd4d4", "#2596e8", "#fafafa", "#ffd166"];
  let lastBurst = 0;

  const EASE = "cubic-bezier(0.22, 1, 0.36, 1)";

  function fixedDot(x, y, size, extra) {
    const dot = document.createElement("span");
    dot.style.cssText = `position:fixed;left:${x - size / 2}px;top:${y - size / 2}px;width:${size}px;height:${size}px;` +
      `border-radius:50%;pointer-events:none;z-index:100;will-change:transform,opacity;${extra}`;
    document.body.appendChild(dot);
    return dot;
  }

  function celebrate() {
    const now = performance.now();
    if (now - lastBurst < 1400) return;
    lastBurst = now;
    const origin = (badge && !badge.hidden && star ? star : link).getBoundingClientRect();
    const cx = origin.left + origin.width / 2, cy = origin.top + origin.height / 2;
    if (star) {
      star.animate(
        [
          { transform: "scale(1) rotate(0deg)" },
          { transform: "scale(1.35) rotate(40deg)", offset: 0.4 },
          { transform: "scale(1) rotate(72deg)" },
        ],
        { duration: 1100, easing: "cubic-bezier(0.34, 1.4, 0.64, 1)" },
      );
    }
    const ring = fixedDot(cx, cy, 16, "border:1px solid #ffd166;");
    ring.animate(
      [{ transform: "scale(0.4)", opacity: 0.7 }, { transform: "scale(3.2)", opacity: 0 }],
      { duration: 1100, easing: EASE },
    ).onfinish = () => ring.remove();

    const COUNT = 12;
    for (let i = 0; i < COUNT; i++) {
      const size = 3 + Math.random() * 2.5;
      const angle = (i / COUNT) * Math.PI * 2 + Math.random() * 0.35;
      const reach = 22 + Math.random() * 30;
      const dx = Math.cos(angle) * reach, dy = Math.sin(angle) * reach;
      const spark = fixedDot(cx, cy, size, `background:${COLORS[i % COLORS.length]};opacity:0;`);
      spark.animate(
        [
          { transform: "translate(0, 0) scale(0.2)", opacity: 0 },
          { transform: `translate(${dx * 0.55}px, ${dy * 0.55}px) scale(1)`, opacity: 1, offset: 0.3 },
          { transform: `translate(${dx}px, ${dy - 8}px) scale(0.5)`, opacity: 0 },
        ],
        { duration: 1300 + Math.random() * 500, delay: i * 18, easing: EASE, fill: "backwards" },
      ).onfinish = () => spark.remove();
    }
  }

  link.addEventListener("pointerenter", celebrate);
  link.addEventListener("focus", celebrate);
})();

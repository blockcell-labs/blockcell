/* BlockCell site — UI interactions */

/* ---- nav background on scroll ---- */
const nav = document.getElementById('nav');
const onScroll = () => nav.classList.toggle('scrolled', window.scrollY > 24);
onScroll();
window.addEventListener('scroll', onScroll, { passive: true });

/* ---- reveal on scroll ---- */
const io = new IntersectionObserver((entries) => {
  entries.forEach((e) => {
    if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
  });
}, { threshold: 0.12, rootMargin: '0px 0px -40px 0px' });
document.querySelectorAll('.reveal').forEach((el, i) => {
  el.style.transitionDelay = `${(i % 3) * 70}ms`;
  io.observe(el);
});

/* ---- stat counters ---- */
const statIO = new IntersectionObserver((entries) => {
  entries.forEach((e) => {
    if (!e.isIntersecting) return;
    const el = e.target;
    const target = +el.dataset.count;
    const suffix = el.dataset.suffix || '';
    let cur = 0;
    const step = Math.max(1, Math.ceil(target / 45));
    const timer = setInterval(() => {
      cur += step;
      if (cur >= target) { cur = target; clearInterval(timer); }
      el.textContent = cur + suffix;
    }, 24);
    statIO.unobserve(el);
  });
}, { threshold: 0.6 });
document.querySelectorAll('[data-count]').forEach((el) => statIO.observe(el));

/* ---- terminal typing animation ---- */
const term = document.getElementById('terminal');
const lines = [
  { html: '<span class="p">➜ ~</span><span class="cmd">blockcell setup</span>', type: 'cmd', pause: 380 },
  { html: 'Configuring provider and validating workspace...', type: 'out', pause: 260 },
  { html: '<span class="ok">✔</span> Provider configured', type: 'out', pause: 160 },
  { html: '<span class="ok">✔</span> Workspace ready', type: 'out', pause: 420 },
  { html: '<span class="p">➜ ~</span><span class="cmd">blockcell skills install stock_monitor</span>', type: 'cmd', pause: 380 },
  { html: 'Downloading stock_monitor from hub...', type: 'out', pause: 300 },
  { html: '<span class="ok">✔</span> Skill <span class="ver">stock_monitor v1.2.0</span> installed', type: 'out', pause: 420 },
  { html: '<span class="p">➜ ~</span><span class="cmd">blockcell agent</span>', type: 'cmd', pause: 300 },
  { html: '<span class="ver">blockcell v0.1.8</span> — self-evolving agent online', type: 'out', pause: 200 },
  { html: '<span class="ok">✔</span> 60+ tools loaded · evolution loop active', type: 'out', pause: 0, cursor: true },
];

function renderTerminal() {
  let i = 0;
  const next = () => {
    if (i >= lines.length) return;
    const l = lines[i++];
    const div = document.createElement('div');
    div.className = l.type;
    div.innerHTML = l.html + (l.cursor ? '<span class="cursor"></span>' : '');
    term.appendChild(div);
    term.scrollTop = term.scrollHeight;
    setTimeout(next, l.pause || 200);
  };
  setTimeout(next, 500);
}
const termIO = new IntersectionObserver((entries) => {
  entries.forEach((e) => {
    if (e.isIntersecting) { renderTerminal(); termIO.disconnect(); }
  });
}, { threshold: 0.3 });
termIO.observe(term);

/* ---- copy install command ---- */
const copyBtn = document.getElementById('copyBtn');
if (copyBtn) {
  copyBtn.addEventListener('click', async () => {
    const cmd = 'curl -fsSL https://blockcell.dev/install.sh | sh';
    try {
      await navigator.clipboard.writeText(cmd);
      copyBtn.textContent = '已复制 ✓';
      setTimeout(() => (copyBtn.textContent = '复制'), 1800);
    } catch {
      copyBtn.textContent = '请手动复制';
      setTimeout(() => (copyBtn.textContent = '复制'), 1800);
    }
  });
}

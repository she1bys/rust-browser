// Reader mode: shows the page's article as clean, readable text on top of the
// page. Run in the page by Rust (`Browser::toggle_reader`); running it again
// closes the reader. Returns "opened", "closed" or "none" (no article found).
//
// How the article is found: every paragraph with real text gives points to
// its parent (and half to its grandparent), minus a penalty for text that is
// mostly links (menus, related-story lists). The block with the most points
// is the article. Only text, headings, lists, quotes, code, images and links
// are copied over, as fresh elements without the page's scripts or styles.
(() => {
  const HOST_ID = '__rust_browser_reader__';
  const open = document.getElementById(HOST_ID);
  if (open) {
    open.__close();
    return 'closed';
  }

  const textOf = (el) => (el.textContent || '').replace(/\s+/g, ' ').trim();
  const linkText = (el) => {
    let n = 0;
    for (const a of el.querySelectorAll('a')) n += textOf(a).length;
    return n;
  };
  const SKIP = 'nav, aside, footer, header nav, form, button, script, style, noscript, iframe, svg, [aria-hidden="true"], [hidden], .ad, .ads, .advert, .share, .social, .related, .comments, #comments';

  // ---- score blocks -------------------------------------------------------
  const scores = new Map();
  const add = (el, points) => { if (el && el !== document.documentElement) scores.set(el, (scores.get(el) || 0) + points); };
  for (const p of document.querySelectorAll('p, pre, blockquote, li')) {
    if (p.closest(SKIP)) continue;
    const len = textOf(p).length;
    if (len < 40) continue;
    const points = 1 + Math.min(len, 600) / 100 + (textOf(p).split(',').length - 1) * 0.3;
    add(p.parentElement, points);
    add(p.parentElement && p.parentElement.parentElement, points / 2);
  }
  let best = null;
  let bestScore = 0;
  for (const [el, score] of scores) {
    const total = textOf(el).length || 1;
    const adjusted = score * (1 - Math.min(linkText(el) / total, 0.9));
    if (adjusted > bestScore) { best = el; bestScore = adjusted; }
  }
  // Prefer a surrounding <article> when the best block sits inside one.
  if (best) {
    const article = best.closest('article');
    if (article && textOf(article).length < textOf(best).length * 3) best = article;
  }
  if (!best || textOf(best).length < 500) return 'none';

  // ---- copy the article as clean elements ----------------------------------
  const BLOCKS = new Set(['P', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'UL', 'OL', 'LI', 'BLOCKQUOTE', 'PRE', 'FIGURE', 'FIGCAPTION', 'TABLE', 'THEAD', 'TBODY', 'TR', 'TD', 'TH', 'HR', 'BR', 'DL', 'DT', 'DD']);
  const INLINE = new Set(['A', 'EM', 'I', 'STRONG', 'B', 'CODE', 'SUB', 'SUP', 'SMALL', 'MARK', 'S', 'U', 'Q', 'ABBR', 'TIME', 'SPAN']);
  const pageTitle = textOf(document.querySelector('h1') || document.createElement('h1')) || document.title;

  function copy(node, out) {
    for (const child of node.childNodes) {
      if (child.nodeType === Node.TEXT_NODE) {
        if (child.textContent) out.append(document.createTextNode(child.textContent));
        continue;
      }
      if (child.nodeType !== Node.ELEMENT_NODE || child.matches(SKIP)) continue;
      const tag = child.tagName;
      if (tag === 'IMG' || tag === 'PICTURE') {
        const img = tag === 'IMG' ? child : child.querySelector('img');
        const src = img && (img.currentSrc || img.src || img.dataset.src || '');
        const small = img && ((img.naturalWidth && img.naturalWidth < 120) || (img.width && img.width < 120));
        if (src && /^https?:/i.test(src) && !small) {
          const clean = document.createElement('img');
          clean.src = src;
          clean.alt = img.alt || '';
          clean.loading = 'lazy';
          out.append(clean);
        }
        continue;
      }
      if (tag === 'H1' && textOf(child) === pageTitle) continue; // shown as the title
      if (BLOCKS.has(tag) || INLINE.has(tag)) {
        const clean = document.createElement(tag === 'SPAN' ? 'span' : tag.toLowerCase());
        if (tag === 'A' && child.href && /^https?:/i.test(child.href)) clean.href = child.href;
        copy(child, clean);
        if (clean.textContent.trim() || clean.querySelector('img') || tag === 'BR' || tag === 'HR') out.append(clean);
      } else {
        copy(child, out); // a <div>, <section> etc.: keep its contents
      }
    }
  }
  const body = document.createElement('div');
  body.className = 'body';
  copy(best, body);

  // ---- show it ---------------------------------------------------------------
  const host = document.createElement('div');
  host.id = HOST_ID;
  Object.assign(host.style, { position: 'fixed', inset: '0', zIndex: '2147483647', display: 'block' });
  const root = host.attachShadow({ mode: 'closed' });
  const css = `
    :host { all: initial; }
    .page { position: absolute; inset: 0; overflow-y: auto; background: var(--bg); color: var(--fg);
      font: 400 var(--size, 19px)/1.7 Georgia, 'Iowan Old Style', 'Palatino Linotype', serif; }
    .page.light { --bg: #fbfbf8; --fg: #1f2328; --muted: #6b7280; --link: #1d4ed8; --rule: #e5e7eb; --bar: rgba(251,251,248,.92); }
    .page.sepia { --bg: #f4ecd8; --fg: #3b2f20; --muted: #7a6a52; --link: #8a4b12; --rule: #e2d5b8; --bar: rgba(244,236,216,.92); }
    .page.dark  { --bg: #18191c; --fg: #e3e3e0; --muted: #9a9b9f; --link: #8ab4ff; --rule: #2e3035; --bar: rgba(24,25,28,.92); }
    .bar { position: sticky; top: 0; display: flex; align-items: center; gap: 6px; padding: 10px 16px;
      background: var(--bar); border-bottom: 1px solid var(--rule); font: 500 13px 'Segoe UI', system-ui, sans-serif; z-index: 1; }
    .bar .site { flex: 1; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .bar button { height: 30px; min-width: 30px; padding: 0 10px; border: 1px solid var(--rule); border-radius: 8px;
      background: transparent; color: var(--fg); font: inherit; cursor: pointer; }
    .bar button:hover { background: var(--rule); }
    .bar button.on { outline: 2px solid var(--link); outline-offset: -2px; }
    .bar .sep { width: 1px; height: 20px; background: var(--rule); margin: 0 4px; }
    article { max-width: 680px; margin: 0 auto; padding: 40px 24px 120px; }
    h1.title { font: 700 calc(var(--size, 19px) * 1.75)/1.2 'Segoe UI', system-ui, sans-serif; margin: 0 0 8px; letter-spacing: -0.01em; }
    .meta { color: var(--muted); font: 400 14px 'Segoe UI', system-ui, sans-serif; margin-bottom: 32px; }
    .body h2, .body h3, .body h4 { font-family: 'Segoe UI', system-ui, sans-serif; line-height: 1.3; margin: 1.6em 0 .5em; }
    .body p, .body ul, .body ol, .body blockquote, .body pre, .body figure, .body table { margin: 0 0 1.1em; }
    .body img { display: block; max-width: 100%; height: auto; margin: 1.2em auto; border-radius: 6px; }
    .body figcaption { color: var(--muted); font-size: .8em; text-align: center; }
    .body a { color: var(--link); }
    .body blockquote { border-left: 3px solid var(--rule); padding-left: 1em; color: var(--muted); }
    .body pre, .body code { font-family: Consolas, 'Cascadia Mono', monospace; font-size: .85em; }
    .body pre { overflow-x: auto; padding: 12px; background: var(--rule); border-radius: 6px; }
    .body table { border-collapse: collapse; } .body td, .body th { border: 1px solid var(--rule); padding: 4px 8px; }
    .body hr { border: 0; border-top: 1px solid var(--rule); }
  `;
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(css);
    root.adoptedStyleSheets = [sheet];
  } catch (_) {
    const style = document.createElement('style');
    style.textContent = css;
    root.append(style);
  }

  const dark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
  const page = document.createElement('div');
  page.className = 'page ' + (dark ? 'dark' : 'light');
  let size = 19;
  page.style.setProperty('--size', size + 'px');

  const bar = document.createElement('div');
  bar.className = 'bar';
  const site = document.createElement('span');
  site.className = 'site';
  site.textContent = location.hostname.replace(/^www\./, '');
  const button = (label, title, action) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.title = title;
    b.setAttribute('aria-label', title);
    b.addEventListener('click', action);
    return b;
  };
  const sep = () => Object.assign(document.createElement('span'), { className: 'sep' });
  const resize = (step) => { size = Math.max(14, Math.min(30, size + step)); page.style.setProperty('--size', size + 'px'); };
  const themes = {};
  const setTheme = (name) => {
    page.className = 'page ' + name;
    for (const [key, b] of Object.entries(themes)) b.classList.toggle('on', key === name);
  };
  themes.light = button('Light', 'Light background', () => setTheme('light'));
  themes.sepia = button('Sepia', 'Sepia background', () => setTheme('sepia'));
  themes.dark = button('Dark', 'Dark background', () => setTheme('dark'));
  setTheme(dark ? 'dark' : 'light');
  bar.append(
    site,
    button('A−', 'Smaller text', () => resize(-1)),
    button('A+', 'Larger text', () => resize(1)),
    sep(), themes.light, themes.sepia, themes.dark, sep(),
    button('✕ Close', 'Close reader mode (Esc)', () => host.__close()),
  );

  const article = document.createElement('article');
  const title = document.createElement('h1');
  title.className = 'title';
  title.textContent = pageTitle;
  const words = textOf(body).split(' ').length;
  const meta = document.createElement('div');
  meta.className = 'meta';
  meta.textContent = Math.max(1, Math.round(words / 230)) + ' min read';
  article.append(title, meta, body);
  page.append(bar, article);
  root.append(page);

  const previousOverflow = document.documentElement.style.overflow;
  const onKey = (e) => { if (e.key === 'Escape') host.__close(); };
  host.__close = () => {
    document.removeEventListener('keydown', onKey, true);
    document.documentElement.style.overflow = previousOverflow;
    host.remove();
  };
  document.addEventListener('keydown', onKey, true);
  document.documentElement.style.overflow = 'hidden';
  (document.body || document.documentElement).append(host);
  page.tabIndex = -1;
  page.focus();
  return 'opened';
})();

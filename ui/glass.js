// Liquid Glass for the browser's own pages: the tab strip and toolbar, the
// panels, the New Tab page.
//
// The optics are a port of liquid-rust (https://github.com/al1h3n/liquid-rust,
// Copyright (c) 2026 al1h3n, used under the MIT License; see
// THIRD-PARTY-NOTICES.md), which models Apple's Liquid Glass on Apple's own
// design kit. That library renders with its own GPU pipeline; here the same model runs
// on the web engine's compositor instead, so it costs no extra libraries, no
// extra process and nothing while the screen holds still:
//
// - Refraction: the backdrop is bent through a squircle bevel (Snell's law,
//   index 1.5), with red and blue split a little apart (dispersion). Each glass
//   shape gets a displacement map computed here, used by an SVG
//   feDisplacementMap in its `backdrop-filter`.
// - Frost: a blur before the bend, as in the original.
// - Tone: the kit's luminosity, lighten and darken fills, as an SVG colour
//   matrix, with a light and a dark face that follow the colour scheme.
// - Thickness: larger glass is frostier, deeper, more opaque and casts a
//   bigger shadow, exactly as the original resolves its materials.
// - Light: a crisp rim line lit from above, inner rims, a darkened hairline
//   edge and a drop shadow (CSS shadows, set per element as `--lg-rims` and
//   `--lg-shadow`; see glass.css).
//
// An element opts in with a `data-glass` attribute ("regular", "clear" or
// "bar"; empty means regular), and `data-glass-press` for the press glow.
// It is all switched on by the `lg` class on <html> and stays off in
// efficiency mode (`efficient` or `lite` on <body>); while it is on, <html>
// has the class `liquid`, which glass.css keys on.
(() => {
  'use strict';
  if (window.liquidGlass) return;

  // ---- the model (liquid-rust: material.rs, scene.rs, glass.wgsl) ---------

  const ETA = 1.5; // index of refraction of the virtual glass
  // Apple's continuous corners (Figma smoothing 60 %) as a superellipse:
  // exponent 2 + 0.95 s + 0.6 s² (scene.rs, smooth_corner).
  const SQUIRCLE_EXPONENT = 2 + 0.95 * 0.6 + 0.6 * 0.6 * 0.6;
  // The same corner in CSS: `corner-shape: superellipse(k)` draws exponent 2^k.
  const SQUIRCLE_CSS = 'superellipse(' + Math.log2(SQUIRCLE_EXPONENT).toFixed(3) + ')';

  const tone = (lift, luminance, mix, lighten, darkenTo, darken) => ({ lift, luminance, mix, lighten, darkenTo, darken });
  const PRESETS = {
    // Glass.regular: bars, buttons, menus, anything with text.
    regular: {
      refraction: 0.7, depth: 30, dispersion: 0.2, frost: 10, specular: 0.3, rim: 0.157, rimShade: 0.05, edge: 0.2,
      shadow: { y: 8, blur: 15, opacity: 0.06 },
      light: tone(0, 0.92, 0.3, 0.2, 0.75, 0.1),
      dark: tone(0.03, 0.25, 0.2, 0, 0, 0.25),
      adaptive: true,
    },
    // Glass.clear: Apple kit "Clear Glass", for controls over busy content.
    clear: {
      refraction: 0.7, depth: 30, dispersion: 0.2, frost: 6, specular: 0.4, rim: 0.157, rimShade: 0.098, edge: 0.2,
      shadow: { y: 8, blur: 15, opacity: 0.02 },
      light: tone(0.063, 1, 0.04, 0, 1, 0),
      dark: tone(0.063, 1, 0.04, 0, 1, 0),
      adaptive: false,
    },
    // Clear chrome (the kit's Dock and search pill): light frost, crisp lines.
    bar: {
      refraction: 0.7, depth: 30, dispersion: 0.2, frost: 3, specular: 0.2, rim: 0.157, rimShade: 0, edge: 0.2,
      shadow: { y: 8, blur: 15, opacity: 0.02 },
      light: tone(0, 0.6, 0.33, 0, 1, 0),
      dark: tone(0, 0.6, 0.33, 0, 1, 0),
      adaptive: false,
    },
  };

  const clamp = (x, lo, hi) => Math.min(hi, Math.max(lo, x));
  const smoothstep = (e0, e1, x) => {
    const t = clamp((x - e0) / (e1 - e0), 0, 1);
    return t * t * (3 - 2 * t);
  };
  const moreOpaque = (amount, k) => (k >= 0 ? amount + (1 - amount) * k : amount * (1 + k));

  // Material::resolve: larger glass is thicker. Reduce Transparency makes it
  // frostier and as opaque as fully tinted glass.
  function resolve(name, minSide, reduceTransparency) {
    const p = PRESETS[name] || PRESETS.regular;
    const m = { ...p, shadow: { ...p.shadow }, light: { ...p.light }, dark: { ...p.dark } };
    if (m.adaptive) {
      const tau = smoothstep(44, 320, minSide);
      m.frost *= 1 + 0.6 * tau;
      m.depth *= 1 + 0.25 * tau;
      m.specular -= 0.05 * tau;
      m.shadow.blur += 33 * tau;
      m.shadow.opacity += 0.19 * tau;
      m.light.lighten += 0.55 * tau;
      m.light.mix += 0.15 * tau;
      m.dark.darken += 0.27 * tau;
    }
    if (reduceTransparency) {
      m.frost = Math.max(m.frost * 3, 20) * Math.pow(2, 1.1);
      m.dark.darkenTo += (0x1c / 255 - m.dark.darkenTo);
      m.dark.darken = Math.min(moreOpaque(m.dark.darken, 0.6), Math.max(0.88, m.dark.darken));
      m.light.lighten = Math.min(moreOpaque(m.light.lighten, 0.6), Math.max(0.88, m.light.lighten));
    }
    return m;
  }

  // Squircle bevel h(t) = (1 - (1 - t)^4)^(1/4): its slope at t (0 = the edge,
  // 1 = where the flat top starts).
  function bevelSlope(t) {
    const u = 1 - t;
    const u3 = u * u * u;
    return u3 * Math.pow(Math.max(1 - u3 * u, 1e-5), -0.75);
  }

  // Sideways travel, in glass thicknesses, of a ray refracted by a surface
  // tilted by atan(slope).
  function refractOffset(slope, eta) {
    const theta = Math.atan(slope);
    return Math.tan(theta - Math.asin(clamp(Math.sin(theta) / eta, -1, 1)));
  }

  // Distance to the edge of a rounded box (negative inside) and the outward
  // normal. Corners are superellipses with exponent `n` reaching `r` along
  // each side; along the straight parts the normal turns smoothly over `soft`
  // px, so the bevel has no crease at the corners (glass.wgsl, sdg_rrect).
  function distance(px, py, hx, hy, r, n, soft) {
    const sx = px < 0 ? -1 : 1;
    const sy = py < 0 ? -1 : 1;
    const wx = Math.abs(px) - (hx - r);
    const wy = Math.abs(py) - (hy - r);
    const g = Math.max(wx, wy);
    if (g > 0) {
      const qx = Math.max(wx, 1e-7);
      const qy = Math.max(wy, 1e-7);
      if (n === 2) {
        const l = Math.hypot(qx, qy);
        return [l - r, (sx * qx) / l, (sy * qy) / l];
      }
      const l = Math.pow(Math.pow(qx, n) + Math.pow(qy, n), 1 / n);
      const gx = Math.pow(qx / l, n - 1);
      const gy = Math.pow(qy / l, n - 1);
      const gl = Math.hypot(gx, gy);
      return [(l - r) / gl, (sx * gx) / gl, (sy * gy) / gl];
    }
    const s = Math.max(soft, 1e-3);
    const ex = Math.exp((wx - g) / s);
    const ey = Math.exp((wy - g) / s);
    const el = Math.hypot(ex, ey);
    return [g - r, (sx * ex) / el, (sy * ey) / el];
  }

  // The displacement map of one shape: for every pixel, where the light that
  // reaches it comes from (red = x, green = y, 128 = straight through).
  // feDisplacementMap moves each pixel by `scale` * (channel - 0.5).
  function displacementMap(w, h, r, n, depth, refraction) {
    // The map is smooth, so big glass needs only half the pixels.
    const res = Math.max(w, h) > 360 ? 0.5 : 1;
    const cw = Math.max(1, Math.round(w * res));
    const ch = Math.max(1, Math.round(h * res));
    const reach = refraction * depth;
    const scale = 2 * reach * refractOffset(bevelSlope(0), ETA) + 2;
    const canvas = document.createElement('canvas');
    canvas.width = cw;
    canvas.height = ch;
    const ctx = canvas.getContext('2d');
    const image = ctx.createImageData(cw, ch);
    const data = image.data;
    const hx = w / 2;
    const hy = h / 2;
    const soft = depth * 0.35;
    for (let j = 0; j < ch; j++) {
      const py = (j + 0.5) / res - hy;
      for (let i = 0; i < cw; i++) {
        const px = (i + 0.5) / res - hx;
        const k = (j * cw + i) * 4;
        let dx = 0;
        let dy = 0;
        // Deep inside, the glass is flat: no need to measure.
        if (Math.abs(px) > hx - depth - 1 || Math.abs(py) > hy - depth - 1) {
          const [d, nx, ny] = distance(px, py, hx, hy, r, n, soft);
          if (d < 0) {
            const t = Math.min(-d / depth, 1);
            if (t < 1) {
              // Light reaching the edge comes from further in: the rim magnifies.
              const off = reach * refractOffset(bevelSlope(t), ETA);
              dx = -nx * off;
              dy = -ny * off;
            }
          }
        }
        data[k] = clamp(Math.round(128 + (dx / scale) * 255), 0, 255);
        data[k + 1] = clamp(Math.round(128 + (dy / scale) * 255), 0, 255);
        data[k + 2] = 128;
        data[k + 3] = 255;
      }
    }
    ctx.putImageData(image, 0, 0);
    return { url: canvas.toDataURL('image/png'), scale };
  }

  // The tone fills of one face as a colour matrix: lift (linear dodge), then
  // the luminosity fill, then the white lighten fill. All of it is linear in
  // sRGB-encoded values, like Figma's blend modes.
  function toneMatrix(t) {
    const W = [0.3, 0.59, 0.11];
    const keep = 1 - t.lighten;
    const offset = keep * (t.lift * (1 - t.mix) + t.mix * t.luminance) + t.lighten;
    const rows = [];
    for (let i = 0; i < 3; i++) {
      const row = [0, 1, 2].map((j) => keep * ((i === j ? 1 : 0) - t.mix * W[j]));
      rows.push(...row, 0, offset);
    }
    rows.push(0, 0, 0, 1, 0);
    return rows.map((v) => +v.toFixed(5)).join(' ');
  }

  // The darken fill: min(x, darkenTo), mixed in by `darken`. Not linear, so it
  // is a lookup table.
  function darkenTable(t) {
    const values = [];
    for (let k = 0; k <= 32; k++) {
      const x = k / 32;
      values.push(+((1 - t.darken) * x + t.darken * Math.min(x, t.darkenTo)).toFixed(4));
    }
    return values.join(' ');
  }

  // ---- filters ------------------------------------------------------------

  const SVG_NS = 'http://www.w3.org/2000/svg';
  let host = null; // hidden <svg> that holds the filters
  const filters = new Map(); // key -> { id, node, refs }
  const maps = new Map(); // geometry key -> { url, scale }
  let nextId = 1;

  function svgHost() {
    if (!host || !host.isConnected) {
      host = document.createElementNS(SVG_NS, 'svg');
      host.setAttribute('aria-hidden', 'true');
      host.setAttribute('width', '0');
      host.setAttribute('height', '0');
      host.style.cssText = 'position:absolute;width:0;height:0;overflow:hidden;pointer-events:none';
      document.body.append(host);
    }
    return host;
  }

  function cachedMap(w, h, r, n, depth, refraction) {
    const key = [w, h, r, n, depth, refraction].map((v) => +v.toFixed(2)).join(',');
    let map = maps.get(key);
    if (!map) {
      map = displacementMap(w, h, r, n, depth, refraction);
      maps.set(key, map);
      // Keep only the recent ones; old sizes (a window resized) are dropped.
      if (maps.size > 48) maps.delete(maps.keys().next().value);
    }
    return map;
  }

  function prim(parent, name, attrs) {
    const node = document.createElementNS(SVG_NS, name);
    for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
    parent.append(node);
    return node;
  }

  function buildFilter(id, g, m, face) {
    const { w, h } = g;
    const filter = document.createElementNS(SVG_NS, 'filter');
    filter.id = id;
    for (const [k, v] of Object.entries({
      x: 0, y: 0, width: w, height: h, filterUnits: 'userSpaceOnUse', 'color-interpolation-filters': 'sRGB',
    })) filter.setAttribute(k, String(v));
    let out = 'SourceGraphic';
    if (m.refraction > 0) {
      const map = cachedMap(w, h, g.r, g.n, g.depth, m.refraction);
      prim(filter, 'feImage', { href: map.url, x: 0, y: 0, width: w, height: h, preserveAspectRatio: 'none', result: 'map' });
      const shift = (scale, result) => prim(filter, 'feDisplacementMap', {
        in: 'SourceGraphic', in2: 'map', scale: +scale.toFixed(3), xChannelSelector: 'R', yChannelSelector: 'G', result,
      });
      // Dispersion: red bends a little less than green, blue a little more.
      // Only worth three passes on larger glass, where it can be seen.
      const disp = m.dispersion * 0.25;
      if (disp > 0 && w * h >= 12000) {
        const s = bevelSlope(0.3);
        const base = refractOffset(s, ETA);
        shift(map.scale * (refractOffset(s, ETA - disp) / base), 'r');
        shift(map.scale, 'g');
        shift(map.scale * (refractOffset(s, ETA + disp) / base), 'b');
        prim(filter, 'feColorMatrix', { in: 'r', values: '1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0', result: 'rr' });
        prim(filter, 'feColorMatrix', { in: 'g', values: '0 0 0 0 0 0 1 0 0 0 0 0 0 0 0 0 0 0 1 0', result: 'gg' });
        prim(filter, 'feColorMatrix', { in: 'b', values: '0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 0 0 0 1 0', result: 'bb' });
        prim(filter, 'feComposite', { in: 'rr', in2: 'gg', operator: 'arithmetic', k1: 0, k2: 1, k3: 1, k4: 0, result: 'rg' });
        prim(filter, 'feComposite', { in: 'rg', in2: 'bb', operator: 'arithmetic', k1: 0, k2: 1, k3: 1, k4: 0, result: 'bent' });
      } else {
        shift(map.scale, 'bent');
      }
      out = 'bent';
    }
    prim(filter, 'feColorMatrix', { in: out, type: 'matrix', values: toneMatrix(face), result: 'toned' });
    if (face.darken > 0) {
      const transfer = prim(filter, 'feComponentTransfer', { in: 'toned' });
      for (const c of ['feFuncR', 'feFuncG', 'feFuncB']) prim(transfer, c, { type: 'table', tableValues: darkenTable(face) });
    }
    return filter;
  }

  function acquire(key, g, m, face) {
    let entry = filters.get(key);
    if (!entry) {
      const id = 'lg-' + nextId++;
      const node = buildFilter(id, g, m, face);
      svgHost().append(node);
      entry = { id, node, refs: 0 };
      filters.set(key, entry);
    }
    entry.refs++;
    return entry;
  }

  function release(key) {
    const entry = key && filters.get(key);
    if (!entry) return;
    entry.refs--;
    if (entry.refs <= 0) {
      entry.node.remove();
      filters.delete(key);
    }
  }

  // ---- light: rims, edge and shadow (CSS) ---------------------------------

  function rims(m, dark) {
    const white = (a) => 'rgb(255 255 255 / ' + clamp(a, 0, 1).toFixed(3) + ')';
    const black = (a) => 'rgb(0 0 0 / ' + clamp(a, 0, 1).toFixed(3) + ')';
    // The specular line: crisp, brightest on the edge facing the light (from
    // above), weaker on the opposite edge (Apple lights glass from two sides).
    const spec = m.specular * (dark ? 1.6 : 2.5);
    return [
      'inset 0 1px 0 -0.25px ' + white(spec),
      'inset 0 -1px 0 -0.25px ' + white(spec * 0.4),
      'inset 1px 0 0 -0.5px ' + white(spec * 0.45),
      'inset -1px 0 0 -0.5px ' + white(spec * 0.45),
      // The kit's inner rims (#282828 dodge) at the lit and opposite edges...
      'inset 0 3px 10px -5px ' + white(m.rim * (dark ? 1.4 : 2.4)),
      'inset 0 -3px 10px -5px ' + white(m.rim * (dark ? 0.9 : 1.6)),
      // ...and the soft shade just under the lit edge (#E6E6E6 burn).
      'inset 0 6px 14px -10px ' + black(m.rimShade * 1.5),
    ].join(', ');
  }

  function shadow(m, dark) {
    // iOS 27's darkened hairline edge defines glass against light content; on
    // dark content Apple's edge reads as a faint light line instead.
    const edge = dark ? '0 0 0 0.5px rgb(255 255 255 / 0.10)' : '0 0 0 0.5px rgb(0 0 0 / ' + (m.edge * 0.5).toFixed(3) + ')';
    const s = m.shadow;
    const opacity = s.opacity * (dark ? 2.2 : 1);
    return edge + ', 0 ' + s.y.toFixed(1) + 'px ' + s.blur.toFixed(1) + 'px rgb(0 0 0 / ' + opacity.toFixed(3) + ')';
  }

  // ---- elements -----------------------------------------------------------

  const darkQuery = matchMedia('(prefers-color-scheme: dark)');
  const reduceQuery = matchMedia('(prefers-reduced-transparency: reduce)');
  const tracked = new Map(); // element -> filter key
  const pending = new Set();
  let frame = 0;
  let active = false;

  const resizes = new ResizeObserver((entries) => {
    for (const entry of entries) schedule(entry.target);
  });

  function schedule(el) {
    pending.add(el);
    if (!frame) frame = requestAnimationFrame(flush);
  }

  // At most this much work per frame: a panel full of new glass (Settings)
  // builds over a few frames instead of holding one up.
  const FRAME_BUDGET_MS = 8;

  function flush() {
    frame = 0;
    const start = performance.now();
    for (const el of [...pending]) {
      if (performance.now() - start > FRAME_BUDGET_MS) {
        frame = requestAnimationFrame(flush);
        return;
      }
      pending.delete(el);
      if (tracked.has(el)) update(el);
    }
  }

  function clearStyle(el) {
    for (const prop of ['backdrop-filter', 'corner-shape', '--lg-rims', '--lg-shadow']) el.style.removeProperty(prop);
  }

  function update(el) {
    // Layout size: a press scales the element, but not its glass.
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const before = tracked.get(el);
    if (w < 4 || h < 4 || !el.isConnected) {
      release(before);
      tracked.set(el, null);
      clearStyle(el);
      return;
    }
    const style = getComputedStyle(el);
    const minSide = Math.min(w, h);
    const radius = Math.min(parseFloat(style.borderTopLeftRadius) || 0, minSide / 2);
    const capsule = radius >= minSide / 2 - 0.5;
    const dark = darkQuery.matches;
    const reduce = reduceQuery.matches;
    const preset = PRESETS[el.dataset.glass] ? el.dataset.glass : 'regular';
    const m = resolve(preset, minSide, reduce);
    const g = {
      w,
      h,
      r: radius,
      n: capsule ? 2 : SQUIRCLE_EXPONENT,
      // The bevel never reaches past the middle of the shape.
      depth: Math.max(1, Math.min(m.depth, minSide / 2)),
    };
    const face = dark ? m.dark : m.light;
    const key = [preset, w, h, g.r.toFixed(1), g.n, dark, reduce].join('|');
    if (key !== before) {
      const entry = acquire(key, g, m, face);
      release(before);
      tracked.set(el, key);
      // Frost first, then the bend and the tone, as in the original.
      el.style.setProperty('backdrop-filter', 'blur(' + (m.frost / 2).toFixed(2) + 'px) url(#' + entry.id + ')');
    }
    el.style.setProperty('corner-shape', capsule ? 'round' : SQUIRCLE_CSS);
    el.style.setProperty('--lg-rims', rims(m, dark));
    el.style.setProperty('--lg-shadow', shadow(m, dark));
  }

  function attach(el) {
    if (tracked.has(el)) return;
    tracked.set(el, null);
    resizes.observe(el);
    schedule(el);
  }

  function detach(el) {
    release(tracked.get(el));
    tracked.delete(el);
    resizes.unobserve(el);
    pending.delete(el);
    clearStyle(el);
    el.classList.remove('lg-pressed');
  }

  // Glass is on when the page asks for it, and never in efficiency mode.
  function wanted() {
    const body = document.body;
    return document.documentElement.classList.contains('lg')
      && !!body && !body.classList.contains('efficient') && !body.classList.contains('lite');
  }

  function scan() {
    const on = wanted();
    if (on !== active) {
      active = on;
      document.documentElement.classList.toggle('liquid', on);
    }
    if (!on) {
      for (const el of [...tracked.keys()]) detach(el);
      return;
    }
    const now = new Set(document.querySelectorAll('[data-glass]'));
    for (const el of [...tracked.keys()]) if (!now.has(el)) detach(el);
    for (const el of now) attach(el);
  }

  let scanQueued = false;
  function queueScan() {
    if (scanQueued) return;
    scanQueued = true;
    queueMicrotask(() => {
      scanQueued = false;
      scan();
    });
  }

  function restyleAll() {
    for (const el of tracked.keys()) schedule(el);
  }

  // The press: glass grows a little on a spring and lights up from the finger.
  function pressTarget(e) {
    const el = e.target instanceof Element ? e.target.closest('[data-glass-press]') : null;
    return active && el && tracked.has(el) ? el : null;
  }
  document.addEventListener('pointerdown', (e) => {
    const el = pressTarget(e);
    if (!el || e.button !== 0) return;
    const r = el.getBoundingClientRect();
    el.style.setProperty('--lg-x', (e.clientX - r.left).toFixed(0) + 'px');
    el.style.setProperty('--lg-y', (e.clientY - r.top).toFixed(0) + 'px');
    el.classList.add('lg-pressed');
  }, true);
  const unpress = () => {
    for (const el of document.querySelectorAll('.lg-pressed')) el.classList.remove('lg-pressed');
  };
  for (const type of ['pointerup', 'pointercancel', 'dragstart']) document.addEventListener(type, unpress, true);
  window.addEventListener('blur', unpress);

  function start() {
    new MutationObserver(queueScan).observe(document.documentElement, {
      subtree: true, childList: true, attributes: true, attributeFilter: ['data-glass', 'class'],
    });
    darkQuery.addEventListener('change', restyleAll);
    reduceQuery.addEventListener('change', restyleAll);
    scan();
  }

  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start, { once: true });

  window.liquidGlass = {
    refresh: () => { scan(); restyleAll(); },
    // For tests: the model's pieces.
    model: { resolve, bevelSlope, refractOffset, distance, toneMatrix, SQUIRCLE_EXPONENT },
  };
})();

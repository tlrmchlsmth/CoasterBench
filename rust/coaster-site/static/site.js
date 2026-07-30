// Facets, rotators, strips, chart tooltips, lightbox, replays. One delegated
// click handler plus a few small modules; no framework, no bundle.
function applyFacets(active) {
  var shown = 0;
  document.querySelectorAll('.run-table tbody tr').forEach(function (row) {
    var show = true;
    if (active.mode && row.dataset.mode !== active.mode) show = false;
    if (active.coaster && row.dataset.coaster !== active.coaster) show = false;
    if (active.harness && row.dataset.harness !== active.harness) show = false;
    if (active.model && row.dataset.model !== active.model) show = false;
    row.style.display = show ? '' : 'none';
    if (show) shown++;
  });
  // A group or a whole board with nothing left in it should go, not sit there
  // as an empty table with a heading.
  document.querySelectorAll('.board-group').forEach(function (group) {
    group.hidden = !group.querySelector('tbody tr:not([style*="display: none"])');
  });
  document.querySelectorAll('.board').forEach(function (board) {
    board.hidden = !board.querySelector('.board-group:not([hidden])');
  });
  var empty = document.getElementById('no-rows');
  if (empty) empty.hidden = shown > 0;
}

// The filtered, sorted view someone is looking at is worth linking to.
function writeState(active) {
  var params = new URLSearchParams();
  Object.keys(active).forEach(function (k) { if (active[k]) params.set(k, active[k]); });
  var sorted = document.querySelector('.run-table th.sorted-asc, .run-table th.sorted-desc');
  if (sorted && !(sorted.dataset.sort === 'score' && sorted.classList.contains('sorted-desc'))) {
    params.set('sort', sorted.dataset.sort);
    params.set('dir', sorted.classList.contains('sorted-asc') ? 'asc' : 'desc');
  }
  var query = params.toString();
  history.replaceState(null, '', query ? '?' + query : location.pathname);
}

document.addEventListener('click', function (e) {
  var fb = e.target.closest('.facet-btn');
  if (fb) {
    document.querySelectorAll('.facet-btn[data-facet="' + fb.dataset.facet + '"]')
      .forEach(function (b) { b.classList.remove('active'); });
    fb.classList.add('active');
    var active = {};
    document.querySelectorAll('.facet-btn.active').forEach(function (b) {
      if (b.dataset.value) active[b.dataset.facet] = b.dataset.value;
    });
    applyFacets(active);
    writeState(active);
    return;
  }

  // Column sorting. The page ships sorted by score, descending; clicking a
  // header re-sorts, clicking the active one flips direction.
  var th = e.target.closest('.run-table th.sortable');
  if (th) {
    var key = th.dataset.sort;
    var table = th.closest('table');
    var wasDesc = th.classList.contains('sorted-desc');
    var wasAsc = th.classList.contains('sorted-asc');
    // First click on a column uses its natural direction (scores high-first,
    // names A-first); clicking the active column flips it.
    var dir = wasDesc ? 'asc' : wasAsc ? 'desc' : (th.dataset.dir || 'desc');
    table.querySelectorAll('th').forEach(function (h) {
      h.classList.remove('sorted-asc', 'sorted-desc');
    });
    th.classList.add(dir === 'asc' ? 'sorted-asc' : 'sorted-desc');

    var body = table.querySelector('tbody');
    var rows = Array.prototype.slice.call(body.rows);
    var sign = dir === 'asc' ? 1 : -1;
    rows.sort(function (a, b) {
      var x = a.dataset[key], y = b.dataset[key];
      var nx = parseFloat(x), ny = parseFloat(y);
      if (!isNaN(nx) && !isNaN(ny)) return sign * (nx - ny);
      return sign * String(x).localeCompare(String(y));
    });
    rows.forEach(function (row) { body.appendChild(row); });
    var active = {};
    document.querySelectorAll('.facet-btn.active').forEach(function (b) {
      if (b.dataset.value) active[b.dataset.facet] = b.dataset.value;
    });
    writeState(active);
    return;
  }

  var strip = e.target.closest('.strip-btn');
  if (strip) {
    var track = strip.parentElement.querySelector('.gallery');
    var card = track.querySelector('figure');
    var step = (card ? card.offsetWidth + 10 : 195) * 2;
    track.scrollBy({ left: strip.classList.contains('strip-next') ? step : -step, behavior: 'smooth' });
    return;
  }

  var btn = e.target.closest('.rot-btn');
  if (!btn) return;
  var rot = btn.closest('.rotator');
  var shots = JSON.parse(rot.dataset.shots);
  var fulls = JSON.parse(rot.dataset.fulls || '[]');
  var step = btn.classList.contains('rot-next') ? 1 : shots.length - 1;
  var i = (parseInt(rot.dataset.i, 10) + step) % shots.length;
  var labels = JSON.parse(rot.dataset.labels);
  rot.dataset.i = i;
  var img = rot.querySelector('img');
  img.src = shots[i];
  if (fulls[i]) img.dataset.full = fulls[i];
  rot.querySelector('.rot-count').textContent = labels[i];
});

// Restore a shared view: facets first, then the sort column.
(function () {
  if (!document.querySelector('.run-table')) return;
  var params = new URLSearchParams(location.search);
  var active = {};
  ['mode', 'coaster', 'harness', 'model'].forEach(function (facet) {
    var want = params.get(facet);
    if (!want) return;
    var btn = document.querySelector('.facet-btn[data-facet="' + facet + '"][data-value="' + CSS.escape(want) + '"]');
    if (!btn) return;
    document.querySelectorAll('.facet-btn[data-facet="' + facet + '"]').forEach(function (b) {
      b.classList.remove('active');
    });
    btn.classList.add('active');
    active[facet] = want;
  });
  if (Object.keys(active).length) applyFacets(active);
  var sort = params.get('sort');
  if (sort) {
    var th = document.querySelector('.run-table th[data-sort="' + CSS.escape(sort) + '"]');
    // Click it, so sorting has one implementation; twice to flip direction.
    if (th) {
      th.click();
      if ((params.get('dir') || 'desc') !== (th.classList.contains('sorted-asc') ? 'asc' : 'desc')) th.click();
    }
  }
})();

// Filters fold away on a narrow screen, unless the view arrived filtered.
(function () {
  var box = document.querySelector('.facet-box');
  if (!box) return;
  var filtered = location.search.length > 1;
  if (window.innerWidth < 700 && !filtered) box.open = false;
})();

// Hide the strip arrows when everything already fits.
function markScrollableStrips() {
  document.querySelectorAll('.strip').forEach(function (strip) {
    var track = strip.querySelector('.gallery');
    strip.dataset.scrollable = track.scrollWidth > track.clientWidth + 4 ? 'true' : 'false';
  });
}
window.addEventListener('resize', markScrollableStrips);
window.addEventListener('load', markScrollableStrips);
markScrollableStrips();

// Trace page: show everything, only tool calls, or only rejected calls.
document.querySelectorAll('[data-trace-filter]').forEach(function (btn) {
  btn.addEventListener('click', function () {
    var want = btn.dataset.traceFilter;
    document.querySelectorAll('[data-trace-filter]').forEach(function (b) {
      b.classList.toggle('active', b === btn);
    });
    document.querySelectorAll('.trace-row').forEach(function (row) {
      var show = want === ''
        || (want === 'tool' && row.dataset.kind === 'tool')
        || (want === 'failed' && row.dataset.failed === 'true');
      row.hidden = !show;
    });
  });
});

// Head-to-head: pick two finished model runs and compare them client-side.
// All contenders are embedded as JSON, so any pairing renders without a fetch,
// and the pick is reflected in ?a=&b= for shareable matchups.
(function () {
  var root = document.getElementById('compare');
  if (!root) return;
  var data = JSON.parse(document.getElementById('contenders').textContent);
  var byId = {};
  data.forEach(function (c) { byId[c.id] = c; });
  var selects = { a: root.querySelector('[data-side="a"]'), b: root.querySelector('[data-side="b"]') };
  var result = document.getElementById('vs-result');

  // Only same-class contenders may fight: comparing library vs design, or
  // wooden vs twister, is apples to oranges (different tasks, different rating
  // scales). A class is the coaster and the mode together (not a scenario
  // park; multi-scenario runs still compare by coaster and mode).
  var classOf = function (c) { return c.coaster + ' · ' + c.mode; };

  function option(c) {
    var o = document.createElement('option');
    o.value = c.id;
    var score = c.score == null ? 'no score' : c.score.toFixed(2);
    o.textContent = c.model + ' · ' + c.run + ' (' + score + ')';
    return o;
  }

  // Side A picks from everything, grouped by class.
  function fillAnchor() {
    var groups = {};
    data.forEach(function (c) { (groups[classOf(c)] = groups[classOf(c)] || []).push(c); });
    Object.keys(groups).sort().forEach(function (klass) {
      var og = document.createElement('optgroup');
      og.label = klass;
      groups[klass].forEach(function (c) { og.appendChild(option(c)); });
      selects.a.appendChild(og);
    });
  }

  // Side B is constrained to A's class, so a mismatch can't be selected.
  // Keeps B's current pick when it is still in-class, else picks the best
  // opponent that is not A itself.
  function fillOpponents() {
    var anchor = byId[selects.a.value];
    var want = selects.b.value;
    selects.b.innerHTML = '';
    var pool = data.filter(function (c) {
      return anchor && classOf(c) === classOf(anchor);
    });
    pool.forEach(function (c) { selects.b.appendChild(option(c)); });
    var keep = pool.some(function (c) { return c.id === want && c.id !== selects.a.value; });
    if (keep) {
      selects.b.value = want;
    } else {
      var other = pool.find(function (c) { return c.id !== selects.a.value; });
      selects.b.value = other ? other.id : selects.a.value;
    }
  }
  fillAnchor();

  var num = function (n) { return n == null ? '—' : (+n).toFixed(2); };
  var intFmt = function (n) { return n == null ? '—' : String(n); };
  var tokens = function (n) {
    if (!n) return '—';
    if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (n >= 1e3) return Math.round(n / 1e3) + 'k';
    return String(Math.round(n));
  };
  var money = function (n) { return n == null ? '—' : '$' + (+n).toFixed(2); };

  // Metrics shown as two-sided bars. dir: 'up' higher wins, 'down' lower wins,
  // null neutral (shown, no winner). fmt formats the printed value.
  var METRICS = [
    { key: 'score', label: 'excitement', dir: 'up', fmt: num, headline: true },
    { key: 'intensity', label: 'intensity', dir: null, fmt: num },
    { key: 'nausea', label: 'nausea', dir: null, fmt: num },
    { key: 'similarity', label: 'similarity to stock', dir: 'down', fmt: num },
    { key: 'airtime', label: 'airtime', dir: 'up', fmt: intFmt },
    { key: 'drops', label: 'drops', dir: 'up', fmt: intFmt },
    { key: 'tokens', label: 'tokens', dir: 'down', fmt: tokens },
    { key: 'cost', label: 'cost', dir: 'down', fmt: money }
  ];

  // Which side wins a metric, or '' for tie/neutral/unknown.
  function winner(m, a, b) {
    if (m.dir == null) return '';
    var va = a[m.key], vb = b[m.key];
    if (va == null && vb == null) return '';
    if (va == null) return 'b';
    if (vb == null) return 'a';
    if (va === vb) return '';
    var aWins = m.dir === 'up' ? va > vb : va < vb;
    return aWins ? 'a' : 'b';
  }

  function card(c, side) {
    var thumb = c.thumb
      ? '<a href="' + c.href + '"><img src="' + c.thumb + '" alt="' + c.model + '"></a>'
      : '<div class="no-preview">no image</div>';
    return '<div class="vs-card vs-' + side + '">' + thumb
      + '<div class="vs-meta"><a class="vs-model" href="' + c.href + '">' + c.model + '</a>'
      + '<span class="dim">' + c.run + ' · ' + c.date + '</span>'
      + '<span class="dim">' + c.coaster + ' · ' + c.mode + ' · ' + c.harness + '</span></div></div>';
  }

  function bar(m, a, b) {
    var va = a[m.key], vb = b[m.key];
    // Fill is proportional so the winning side always reads as the bigger bar:
    // magnitude for higher-is-better and neutral metrics, its inverse for
    // lower-is-better ones (cost, tokens, similarity).
    var weight = function (v) {
      if (v == null) return 0;
      if (m.dir === 'down') return 1 / (Math.max(0, v) + 1e-6);
      return Math.max(0, v);
    };
    var wa = weight(va), wb = weight(vb);
    var total = wa + wb;
    var pa = total > 0 ? (wa / total) * 100 : 50;
    var win = winner(m, a, b);
    return '<div class="vs-metric' + (m.headline ? ' vs-headline' : '') + '">'
      + '<span class="vs-val vs-val-a ' + (win === 'a' ? 'vs-win' : '') + '">' + m.fmt(va) + '</span>'
      + '<span class="vs-label">' + m.label + '</span>'
      + '<span class="vs-val vs-val-b ' + (win === 'b' ? 'vs-win' : '') + '">' + m.fmt(vb) + '</span>'
      + '<div class="vs-track"><span class="vs-fill-a ' + (win === 'a' ? 'vs-win' : '') + '" style="width:' + pa + '%"></span>'
      + '<span class="vs-fill-b ' + (win === 'b' ? 'vs-win' : '') + '" style="width:' + (100 - pa) + '%"></span></div></div>';
  }

  // Overlaid per-round excitement sparkline: which model climbed faster.
  function spark(a, b) {
    var W = 320, H = 70, pad = 6;
    var maxLen = Math.max(a.round_scores.length, b.round_scores.length, 2);
    var maxY = Math.max(10, Math.max.apply(null, a.round_scores.concat(b.round_scores, [0])));
    var line = function (scores, cls) {
      if (scores.length < 2) return '';
      var pts = scores.map(function (s, i) {
        var x = pad + (W - 2 * pad) * (i / (maxLen - 1));
        var y = H - pad - (H - 2 * pad) * (s / maxY);
        return x.toFixed(1) + ',' + y.toFixed(1);
      });
      return '<polyline class="' + cls + '" points="' + pts.join(' ') + '"/>';
    };
    return '<svg class="vs-spark" viewBox="0 0 ' + W + ' ' + H + '" role="img" '
      + 'aria-label="excitement per round">' + line(a.round_scores, 'vs-spark-a')
      + line(b.round_scores, 'vs-spark-b') + '</svg>';
  }

  function verdict(a, b) {
    var sa = a.score, sb = b.score;
    if (sa == null && sb == null) return 'Neither finished a rated coaster.';
    if (sa == null) return b.model + ' wins by default — ' + a.model + ' never rated a ride.';
    if (sb == null) return a.model + ' wins by default — ' + b.model + ' never rated a ride.';
    if (sa === sb) return 'Dead heat at ' + sa.toFixed(2) + ' excitement.';
    var w = sa > sb ? a : b, l = sa > sb ? b : a;
    return w.model + ' takes it, ' + Math.max(sa, sb).toFixed(2) + ' vs ' + Math.min(sa, sb).toFixed(2)
      + ' excitement (+' + Math.abs(sa - sb).toFixed(2) + ').';
  }

  // Each matchup has a pre-generated permalink page (compare-<a>.vs.<b>.html)
  // carrying its own og:image, so a copied address-bar URL unfurls with the
  // right coasters. Crawlers can't run this JS, hence the static file per pair.
  function permalink(aId, bId) { return 'compare-' + aId + '.vs.' + bId + '.html'; }

  function render() {
    var a = byId[selects.a.value], b = byId[selects.b.value];
    if (!a || !b || a.id === b.id) { result.innerHTML = ''; return; }
    result.innerHTML =
      '<div class="vs-cards">' + card(a, 'a') + '<div class="vs-mid">vs</div>' + card(b, 'b') + '</div>'
      + '<p class="vs-verdict">' + verdict(a, b) + '</p>'
      + '<div class="vs-metrics">' + METRICS.map(function (m) { return bar(m, a, b); }).join('') + '</div>'
      + '<div class="vs-spark-wrap"><span class="dim">excitement per round</span>' + spark(a, b) + '</div>';
    // Point the address bar at this matchup's static permalink, which carries
    // its own og:image. Old ?a=&b= deep links still resolve via the load path.
    history.replaceState(null, '', permalink(a.id, b.id));
  }

  // Reflect A's pick, refill B to A's class, then render.
  function update() { fillOpponents(); render(); }

  var params = new URLSearchParams(window.location.search);
  var wantA = params.get('a') || root.dataset.defaultA;
  var wantB = params.get('b') || root.dataset.defaultB;
  if (byId[wantA]) selects.a.value = wantA;
  fillOpponents();
  // Honour B's deep-link only when it shares A's class; otherwise
  // fillOpponents already left B on the best in-class opponent.
  if (byId[wantB] && classOf(byId[wantB]) === classOf(byId[selects.a.value])
      && wantB !== selects.a.value) {
    selects.b.value = wantB;
  }

  selects.a.addEventListener('change', update);
  selects.b.addEventListener('change', render);
  document.getElementById('vs-swap').addEventListener('click', function () {
    // Both sides share a class, so a straight swap stays valid.
    var tmp = selects.a.value; selects.a.value = selects.b.value;
    fillOpponents();
    selects.b.value = tmp;
    render();
  });
  render();
})();

// Chart readout: each point carries its round's numbers as data attributes
// (see chart.rs). Pointer events, so a touch works too.
(function () {
  document.querySelectorAll('.chart').forEach(function (chart) {
    var svg = chart.querySelector('svg');
    if (!svg || !chart.querySelector('.chart-point')) return;
    var tip = document.createElement('div');
    tip.className = 'chart-tip';
    chart.appendChild(tip);

    function show(point) {
      var d = point.dataset;
      var rated = d.excitement !== '';
      tip.innerHTML = '<b>round ' + d.round + '</b>'
        + (rated
          ? 'excitement <b style="display:inline">' + d.excitement + '</b><br>'
            + 'intensity ' + d.intensity + ' · nausea ' + d.nausea
          : d.note || 'no rated ride');
      // The SVG scales, so place from rendered geometry, not the viewBox.
      var box = point.getBoundingClientRect();
      var frame = chart.getBoundingClientRect();
      tip.style.left = (box.left + box.width / 2 - frame.left) + 'px';
      tip.style.top = (box.top - frame.top) + 'px';
      tip.dataset.show = '1';
      point.classList.add('active');
    }
    function hide() {
      tip.dataset.show = '0';
      chart.querySelectorAll('.chart-point.active').forEach(function (p) {
        p.classList.remove('active');
      });
    }

    chart.querySelectorAll('.chart-point').forEach(function (point) {
      point.addEventListener('pointerenter', function () { show(point); });
      point.addEventListener('pointerdown', function () { show(point); });
      point.addEventListener('focus', function () { show(point); });
      point.addEventListener('blur', hide);
    });
    svg.addEventListener('pointerleave', hide);
  });
})();

// Lightbox: the cards are small and the captures are dense pixel art.
(function () {
  var box;
  function open(img) {
    if (!box) {
      box = document.createElement('div');
      box.className = 'lightbox';
      box.innerHTML = '<img alt=""><figcaption></figcaption>';
      box.addEventListener('click', close);
      document.body.appendChild(box);
    }
    // The card shows a downscaled copy; here the original is worth fetching.
    box.querySelector('img').src = img.dataset.full || img.currentSrc || img.src;
    box.querySelector('img').alt = img.alt;
    box.querySelector('figcaption').textContent = img.alt;
    box.dataset.open = '1';
  }
  function close() { if (box) box.dataset.open = '0'; }
  document.addEventListener('click', function (e) {
    var img = e.target.closest('.round img, .gallery img, .vs-card img');
    // A capture inside a link is a navigation, not a zoom.
    if (!img || img.closest('a')) return;
    e.preventDefault();
    open(img);
  });
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape') close(); });
})();

// Replays: only the headline plays by itself, and only while on screen. A model
// page can hold six clips; those stay preload="none" until clicked. Never under
// reduced-motion or Save-Data.
(function () {
  var hero = document.querySelector('.hero video.replay');
  if (!hero || !('IntersectionObserver' in window)) return;
  var still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  var thrifty = navigator.connection && (navigator.connection.saveData
    || /2g/.test(navigator.connection.effectiveType || ''));
  if (still || thrifty) return;

  // Muted makes autoplay legal. Looping is the template's call: a clip cut at
  // the cap would jump forever.
  hero.muted = true;
  hero.setAttribute('playsinline', '');
  new IntersectionObserver(function (entries) {
    entries.forEach(function (entry) {
      if (entry.isIntersecting && entry.intersectionRatio > 0.55) {
        // A rejected promise just leaves the poster up.
        hero.play().catch(function () {});
      } else if (!hero.paused) {
        hero.pause();
      }
    });
  }, { threshold: [0, 0.55] }).observe(hero);
})();

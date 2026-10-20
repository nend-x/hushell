"use strict";

const MD_SYM = {
  alpha: "\u03b1", beta: "\u03b2", gamma: "\u03b3", delta: "\u03b4",
  epsilon: "\u03b5", varepsilon: "\u03b5", zeta: "\u03b6", eta: "\u03b7",
  theta: "\u03b8", vartheta: "\u03d1", iota: "\u03b9", kappa: "\u03ba",
  lambda: "\u03bb", mu: "\u03bc", nu: "\u03bd", xi: "\u03be",
  pi: "\u03c0", varpi: "\u03d6", rho: "\u03c1", sigma: "\u03c3",
  varsigma: "\u03c2", tau: "\u03c4", upsilon: "\u03c5", phi: "\u03c6",
  varphi: "\u03d5", chi: "\u03c7", psi: "\u03c8", omega: "\u03c9",
  Gamma: "\u0393", Delta: "\u0394", Theta: "\u0398", Lambda: "\u039b",
  Xi: "\u039e", Pi: "\u03a0", Sigma: "\u03a3", Upsilon: "\u03a5",
  Phi: "\u03a6", Psi: "\u03a8", Omega: "\u03a9",
  infty: "\u221e", partial: "\u2202", nabla: "\u2207",
  times: "\u00d7", cdot: "\u00b7", div: "\u00f7", pm: "\u00b1", mp: "\u2213",
  leq: "\u2264", le: "\u2264", geq: "\u2265", ge: "\u2265", neq: "\u2260",
  ne: "\u2260", approx: "\u2248", equiv: "\u2261", sim: "\u223c",
  simeq: "\u2243", cong: "\u2245", propto: "\u221d",
  to: "\u2192", rightarrow: "\u2192", gets: "\u2190", leftarrow: "\u2190",
  leftrightarrow: "\u2194", Rightarrow: "\u21d2", Leftarrow: "\u21d0",
  Leftrightarrow: "\u21d4", mapsto: "\u21a6", uparrow: "\u2191",
  downarrow: "\u2193", nearrow: "\u2197",
  in: "\u2208", notin: "\u2209", ni: "\u220b", subset: "\u2282",
  subseteq: "\u2286", supset: "\u2283", supseteq: "\u2287", emptyset: "\u2205",
  forall: "\u2200", exists: "\u2203", nexists: "\u2204", neg: "\u00ac",
  land: "\u2227", lor: "\u2228", oplus: "\u2295", otimes: "\u2297",
  perp: "\u22a5", parallel: "\u2225", angle: "\u2220", measuredangle: "\u2221",
  triangle: "\u25b3", square: "\u25a1", star: "\u22c6", ast: "\u2217",
  circ: "\u2218", bullet: "\u2219", cup: "\u222a", cap: "\u2229",
  setminus: "\u2216", lceil: "\u2308", rceil: "\u2309", lfloor: "\u230a",
  rfloor: "\u230b", langle: "\u27e8", rangle: "\u27e9", ell: "\u2113",
  hbar: "\u210f", Re: "\u211c", Im: "\u2111", aleph: "\u2135", wp: "\u2118",
  sum: "\u2211", prod: "\u220f", coprod: "\u2210", int: "\u222b",
  iint: "\u222c", iiint: "\u222d", oint: "\u222e",
  cdots: "\u22ef", ldots: "\u2026", dots: "\u2026", vdots: "\u22ee",
  prime: "\u2032", degree: "\u00b0", surd: "\u221a", dagger: "\u2020",
  ddagger: "\u2021", S: "\u00a7", P: "\u00b6", copyright: "\u00a9",
};

const MD_OPNAME = ["sin", "cos", "tan", "cot", "sec", "csc", "arcsin",
  "arccos", "arctan", "sinh", "cosh", "tanh", "log", "ln", "lg", "exp",
  "lim", "liminf", "limsup", "min", "max", "sup", "inf", "det", "dim",
  "deg", "gcd", "mod", "pmod", "bmod", "arg", "hom", "ker", "Pr"];

const MD_KW = {
  javascript: "break case catch class const continue debugger default delete do else export extends finally for function if import in instanceof let new of return static super switch this throw try typeof var void while with yield async await get set true false null undefined",
  typescript: "abstract any as asserts bigint boolean break case catch class const continue declare default delete do else enum export extends finally for from function get if implements import in infer instanceof interface is keyof let module namespace never new null number of private protected public readonly return satisfies set static string super switch symbol this throw true false try type typeof undefined union unknown var void while yield",
  python: "and as assert async await break case class continue def del elif else except False finally for from global if import in is lambda match None nonlocal not or pass raise return self True try while with yield",
  rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Static struct super trait true type unsafe use where while",
  c: "auto break case char const continue default do double else enum extern float for goto if int long register return short signed sizeof static struct switch typedef union unsigned void volatile while NULL false true",
  cpp: "alignas alignof auto bool break case catch char class const constexpr continue decltype default delete do double dynamic_cast else enum explicit extern false float for friend goto if inline int long mutable namespace new noexcept nullptr operator private protected public register return short signed sizeof static static_cast struct switch template this throw true try typedef typeid typename union unsigned using virtual void volatile while",
  json: "true false null",
};

function mdEsc(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/* ---- inline LaTeX subset -> HTML ------------------------------------- */

function mdTex(src) {
  let s = src;

  // \frac{a}{b} (a couple of passes for nested fracs)
  for (let k = 0; k < 3; k++) {
    s = s.replace(/\\(?:d|t)?frac\s*\{([^{}]*)\}\s*\{([^{}]*)\}/g,
      (_m, a, b) => '<span class="tx-frac"><span class="tx-num">' + a +
        '</span><span class="tx-den">' + b + "</span></span>");
  }

  // \sqrt[n]{x} and \sqrt{x}
  s = s.replace(/\\sqrt\s*\[([^\]]*)\]\s*\{([^{}]*)\}/g,
    (_m, n, x) => '<span class="tx-sqrt"><span class="tx-root">' + n +
      '</span><span class="tx-sign">\u221a</span><span class="tx-rad">' +
      x + "</span></span>");
  s = s.replace(/\\sqrt\s*\{([^{}]*)\}/g,
    (_m, x) => '<span class="tx-sqrt"><span class="tx-sign">\u221a</span><span class="tx-rad">' +
      x + "</span></span>");

  // \left( .. \right) -> plain delimiters (\left. / \right. vanish)
  s = s.replace(/\\left\s*(\(|\[|\{|\||\.)/g,
    (_m, d) => (d === "." ? "" : d === "{" ? "\\{" : d));
  s = s.replace(/\\right\s*(\)|\]|\}|\||\.)/g,
    (_m, d) => (d === "." ? "" : d === "}" ? "\\}" : d));

  // limits: ^{..} ^x _{..} _x (right-to-left nesting is fine for chat math)
  for (let k = 0; k < 2; k++) {
    s = s.replace(/\^\{([^{}]+)\}/g, "<sup>$1</sup>");
    s = s.replace(/_\{([^{}]+)\}/g, "<sub>$1</sub>");
    s = s.replace(/\^([A-Za-z0-9\u03b1-\u03c9\u0393-\u03a9\u2032\u221e])/g, "<sup>$1</sup>");
    s = s.replace(/_([A-Za-z0-9\u03b1-\u03c9\u0393-\u03a9])/g, "<sub>$1</sub>");
  }

  // big operators with limits
  s = s.replace(/\\(sum|prod|coprod|int|iint|iiint|oint)\b/g,
    (_m, op) => '<span class="tx-bigop">' + (MD_SYM[op] || op) + "</span>");

  // \text{..} \mathrm{..} \mathbf{..} \mathit{..} (before the symbol pass,
  // so their bodies keep their wordy form)
  s = s.replace(/\\(?:text|mathrm|mbox|operatorname)\s*\{([^{}]*)\}/g,
    (_m, b) => '<span class="tx-txt">' + b + "</span>");
  s = s.replace(/\\mathbf\s*\{([^{}]*)\}/g,
    (_m, b) => '<span class="tx-b">' + b + "</span>");
  s = s.replace(/\\mathit\s*\{([^{}]*)\}/g, (_m, b) => "<i>" + b + "</i>");

  // spacing + row/alignment macros
  s = s.replace(/\\[,;!]/g, " ").replace(/\\quad/g, "\u2003").replace(/\\qquad/g, "\u2003\u2003");
  s = s.replace(/\\\\/g, "<br>").replace(/&/g, "");

  // symbols + operator names — unknown macros keep their name (never eat text)
  s = s.replace(/\\([A-Za-z]+)/g, (m, name) => {
    if (MD_SYM[name] != null) return MD_SYM[name];
    if (MD_OPNAME.includes(name)) return '<span class="tx-op">' + name + "</span>";
    return name;
  });

  // leftover \{ \} become braces
  s = s.replace(/\\([{}])/g, "$1");
  return s;
}

/* ---- light code highlighting ------------------------------------------ */

function mdHi(code, lang) {
  const l = (lang || "").toLowerCase();
  const kws = MD_KW[l];
  if (kws == null) return mdEsc(code);
  const kw = new Set(kws.split(" "));
  const cLine = l === "python" ? "#" : "//";
  const block = l !== "python";
  const out = [];
  let i = 0;
  const n = code.length;
  while (i < n) {
    // comments first
    if (code.startsWith(cLine, i)) {
      let j = code.indexOf("\n", i);
      if (j < 0) j = n;
      out.push('<span class="c-com">' + mdEsc(code.slice(i, j)) + "</span>");
      i = j;
      continue;
    }
    if (block && code.startsWith("/*", i)) {
      let j = code.indexOf("*/", i + 2);
      j = j < 0 ? n : j + 2;
      out.push('<span class="c-com">' + mdEsc(code.slice(i, j)) + "</span>");
      i = j;
      continue;
    }
    // strings
    const ch = code[i];
    if (ch === '"' || ch === "'" || ch === "`") {
      let j = i + 1;
      while (j < n && code[j] !== ch) {
        if (code[j] === "\\") j++;
        j++;
      }
      j = Math.min(j + 1, n);
      out.push('<span class="c-str">' + mdEsc(code.slice(i, j)) + "</span>");
      i = j;
      continue;
    }
    if (/[A-Za-z_]/.test(ch)) {
      let j = i;
      while (j < n && /[A-Za-z0-9_]/.test(code[j])) j++;
      const w = code.slice(i, j);
      out.push(kw.has(w) ? '<span class="c-kw">' + w + "</span>" : mdEsc(w));
      i = j;
      continue;
    }
    if (/[0-9]/.test(ch)) {
      let j = i;
      while (j < n && /[0-9a-fA-FxXoO_.]/.test(code[j])) j++;
      out.push('<span class="c-num">' + mdEsc(code.slice(i, j)) + "</span>");
      i = j;
      continue;
    }
    out.push(mdEsc(ch));
    i++;
  }
  return out.join("");
}

/* ---- the renderer ------------------------------------------------------ */

function renderMd(src) {
  const stash = [];
  const put = (html) => {
    stash.push(html);
    return "\x00" + (stash.length - 1) + "\x00";
  };

  let text = String(src == null ? "" : src);

  // 1. protect code and math
  text = text.replace(/```([\w+#.-]*)[ \t]*\r?\n?([\s\S]*?)```/g,
    (_m, lang, code) => {
      const l = (lang || "").toLowerCase();
      const body = mdHi(code.replace(/\r?\n$/, ""), l);
      const chip = l ? '<span class="cb-lang">' + mdEsc(l) + "</span>" : "";
      return put('<div class="cb-block">' + chip + "<pre><code>" + body + "</code></pre></div>");
    });
  text = text.replace(/`([^`\n]+)`/g, (_m, c) => put("<code>" + mdEsc(c) + "</code>"));

  // display math, then inline math (both sides non-delimiter so "$5 and $10"
  // stays plain text)
  text = text.replace(/\$\$([\s\S]+?)\$\$/g,
    (_m, tex) => put('<div class="md-math-d">' + mdTex(mdEsc(tex.trim())) + "</div>"));
  text = text.replace(/(^|[\s(\[])\$([^$\n]+?)\$(?=[\s.,;:!?)\]]|$)/g,
    (_m, pre, tex) => pre + '<span class="md-math-i">' + mdTex(mdEsc(tex)) + "</span>");

  // 2. block pass (line-based)
  const lines = text.split("\n");
  const out = [];
  let para = [];
  let list = null;      // { type: "ul" | "ol", indent }
  let quote = false;
  let table = null;     // { head: [..], rows: [[..]] }

  const flushPara = () => {
    if (para.length) {
      out.push('<p class="md-p">' + para.join("<br>") + "</p>");
      para = [];
    }
  };
  const closeLists = () => {
    if (list) {
      out.push("</" + list.type + ">");
      list = null;
    }
  };
  const openList = (type, indent) => {
    if (list && list.type === type && list.indent <= indent) return;
    closeLists();
    out.push("<" + type + ">");
    list = { type, indent };
  };
  const flushQuote = () => {
    if (quote) {
      out.push("</blockquote>");
      quote = false;
    }
  };
  const flushTable = () => { if (table) { out.push(mdTable(table)); table = null; } };
  const flushAll = () => { flushPara(); closeLists(); flushQuote(); flushTable(); };

  const inline = (s) => {
    let t = mdEsc(s);
    t = t.replace(/\*\*\*([^*\n]+)\*\*\*/g, "<b><i>$1</i></b>");
    t = t.replace(/\*\*([^*\n]+)\*\*/g, "<b>$1</b>");
    t = t.replace(/(^|[^*\w])\*([^*\n]+)\*(?![*\w])/g, "$1<i>$2</i>");
    t = t.replace(/(^|[^_\w])_([^_\n]+)_(?![_\w])/g, "$1<i>$2</i>");
    t = t.replace(/~~([^~\n]+)~~/g, "<s>$1</s>");
    t = t.replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,
      '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
    return t;
  };
  const mdTable = (t) => {
    let h = '<table class="md-table"><thead><tr>';
    for (const c of t.head) h += "<th>" + inline(c) + "</th>";
    h += "</tr></thead><tbody>";
    for (const row of t.rows) {
      h += "<tr>";
      for (const c of row) h += "<td>" + inline(c) + "</td>";
      h += "</tr>";
    }
    return h + "</tbody></table>";
  };
  const isSep = (l) =>
    /^\s*\|(\s*:?-{2,}:?\s*\|)+\s*:?-{2,}:?\s*\|?\s*$/.test(l) ||
    /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)+\|?\s*$/.test(l);
  const cellsOf = (l) =>
    l.replace(/^\s*\|/, "").replace(/\|\s*$/, "").split("|").map((c) => c.trim());

  for (let li = 0; li < lines.length; li++) {
    const line = lines[li].replace(/\s+$/, "");
    let m;

    if (table) {
      if (/^\s*\|.*\S/.test(line)) {
        table.rows.push(cellsOf(line));
        continue;
      }
      flushTable();
    }

    if ((m = line.match(/^(#{1,4})\s+(.*\S)?\s*$/))) {
      flushAll();
      const lv = m[1].length;
      out.push('<h' + lv + ' class="md-h md-h' + lv + '">' + inline(m[2] || "") + "</h' + lv + '>");
      continue;
    }
    if (/^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      flushAll();
      out.push('<hr class="md-hr">');
      continue;
    }
    if (/^\s*>\s?/.test(line)) {
      flushPara(); closeLists(); flushTable();
      if (!quote) {
        out.push('<blockquote class="md-quote">');
        quote = true;
      }
      out.push('<p class="md-p">' + inline(line.replace(/^\s*>\s?/, "")) + "</p>");
      continue;
    }
    flushQuote();

    if ((m = line.match(/^(\s*)[-*+]\s+(.*\S)?\s*$/)) && m[1].length % 2 === 0) {
      flushPara(); flushTable();
      openList("ul", m[1].length);
      out.push("<li>" + inline(m[2] || "") + "</li>");
      continue;
    }
    if ((m = line.match(/^(\s*)\d{1,9}[.)]\s+(.*\S)?\s*$/)) && m[1].length % 2 === 0) {
      flushPara(); flushTable();
      openList("ol", m[1].length);
      out.push("<li>" + inline(m[3] || "") + "</li>");
      continue;
    }

    if (/^\s*\|.*\S/.test(line) && !/^\s*\|?\s*-{2,}/.test(line)) {
      const next = lines[li + 1] || "";
      if (!table && isSep(next)) {
        flushAll();
        table = { head: cellsOf(line), rows: [] };
        li++; // consume the separator line
        continue;
      }
      if (table) {
        table.rows.push(cellsOf(line));
        continue;
      }
    }

    if (line.trim() === "") {
      flushAll();
      continue;
    }
    closeLists();
    para.push(inline(line));
  }
  flushAll();

  // 3. restore protected blocks (two passes: placeholders inside tables)
  let html = out.join("");
  for (let k = 0; k < 2; k++) {
    html = html.replace(/\x00(\d+)\x00/g, (_m, i) => stash[+i]);
  }
  return html;
}

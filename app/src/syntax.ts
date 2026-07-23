// Lexical syntax tokenizer for the source panes (#223). DOM-free + unit-tested (like
// log.ts / csrc.ts / schempick.ts); main.ts renders the tokens. Scope is lexical only —
// identifiers stay `plain`; model-driven name coloring is #225. A whole-file scan carries
// `/* */` block-comment state across line boundaries and returns one token row per line.
//
// Invariant: for every line, concatenating its tokens' `text` reproduces the line exactly.
// `sourceOffsetAt` (via srcoffset.ts `lineColumn`) relies on it to keep the byte-offset
// cross-probe correct now that a line renders as many nodes instead of one text node.

import { isCLanguage } from "./csrc";

export type TokenClass =
  | "keyword"
  | "type"
  | "number"
  | "string"
  | "comment"
  | "directive" // `define, `ifdef, `timescale  (backtick sigil)
  | "systask" //   $display, $finish            (dollar sigil)
  | "operator"
  | "plain";

export interface Token {
  text: string;
  cls: TokenClass;
}

interface Grammar {
  keywords: Set<string>;
  types: Set<string>;
  /** Sigil introducing a directive: "`" for SystemVerilog, "#" for the C preprocessor. */
  directiveSigil: string | null;
  /** `$display`-style system tasks (SystemVerilog only). */
  systask: boolean;
  /** Single-quoted char literals ('a'), C/C++ only — SV spends ' on sized literals. */
  charLiteral: boolean;
  /** A bare ' can open a sized numeric literal ('d5) — SystemVerilog only. */
  tickNumber: boolean;
  /** Numeric-literal matcher, anchored at ^. */
  num: RegExp;
}

const SV_TYPES = new Set([
  "logic", "wire", "reg", "bit", "byte", "int", "integer", "shortint", "longint",
  "time", "real", "shortreal", "genvar", "signed", "unsigned", "void", "string",
  "chandle", "event",
]);

const SV_KEYWORDS = new Set([
  "module", "endmodule", "begin", "end", "if", "else", "case", "casez", "casex",
  "endcase", "default", "for", "while", "repeat", "generate", "endgenerate",
  "assign", "always", "always_ff", "always_comb", "always_latch", "initial",
  "posedge", "negedge", "input", "output", "inout", "parameter", "localparam",
  "function", "endfunction", "task", "endtask", "return", "typedef", "enum",
  "struct", "union", "packed", "interface", "endinterface", "modport", "import",
  "export", "package", "endpackage", "automatic", "static", "const", "unique",
  "priority", "break", "continue",
]);

// C/C++ (#224). One grammar serves both: C++ keywords are a superset, and the harness's
// language tags (c/cpp/cc/cxx/h/hpp) don't reliably distinguish a C header from a C++ one.
const C_TYPES = new Set([
  "void", "char", "short", "int", "long", "float", "double", "signed", "unsigned",
  "bool", "size_t", "ssize_t", "wchar_t", "char16_t", "char32_t", "auto",
  "int8_t", "int16_t", "int32_t", "int64_t",
  "uint8_t", "uint16_t", "uint32_t", "uint64_t",
  "intptr_t", "uintptr_t", "ptrdiff_t", "nullptr_t",
]);

const C_KEYWORDS = new Set([
  "if", "else", "for", "while", "do", "switch", "case", "default", "break",
  "continue", "return", "goto", "sizeof", "typedef", "struct", "union", "enum",
  "static", "extern", "register", "volatile", "const", "inline", "restrict",
  "class", "public", "private", "protected", "virtual", "override", "final",
  "namespace", "using", "template", "typename", "new", "delete", "this",
  "operator", "friend", "explicit", "mutable", "constexpr", "consteval",
  "noexcept", "throw", "try", "catch", "nullptr", "true", "false",
  "static_cast", "dynamic_cast", "const_cast", "reinterpret_cast", "decltype",
]);

// SystemVerilog: sized literal (32'hFF, 'd5) before a plain decimal so it isn't split.
const SV_NUM = /^(?:\d[\d_]*)?'[sS]?[bBoOdDhH][0-9a-fA-FxXzZ_]+|^\d[\d_]*(?:\.\d[\d_]*)?/;
// C/C++: hex and binary before decimal, else 0x1F would lex as 0 then x1F.
const C_NUM =
  /^0[xX][0-9a-fA-F]+[uUlL]*|^0[bB][01]+[uUlL]*|^\d+(?:\.\d*)?(?:[eE][+-]?\d+)?[uUlLfF]*/;

const SV_GRAMMAR: Grammar = {
  keywords: SV_KEYWORDS,
  types: SV_TYPES,
  directiveSigil: "`",
  systask: true,
  charLiteral: false,
  tickNumber: true,
  num: SV_NUM,
};

const C_GRAMMAR: Grammar = {
  keywords: C_KEYWORDS,
  types: C_TYPES,
  directiveSigil: "#",
  systask: false,
  charLiteral: true,
  tickNumber: false,
  num: C_NUM,
};

// A language with no registered grammar still lexes comments/strings/numbers/operators
// but claims no keywords — a safe default rather than mis-coloring an unknown language.
const PLAIN_GRAMMAR: Grammar = {
  keywords: new Set<string>(),
  types: new Set<string>(),
  directiveSigil: null,
  systask: false,
  charLiteral: false,
  tickNumber: false,
  num: C_NUM,
};

const SV_LANGUAGES = new Set(["systemverilog", "verilog", "sv", "v"]);

// `isCLanguage` is reused from csrc.ts rather than re-listing the C tags, so the pane a
// file renders in and the grammar it highlights with can never disagree.
function grammarFor(lang: string | null | undefined): Grammar {
  if (lang == null) return SV_GRAMMAR; // absent language ⇒ SystemVerilog (SourceFile convention)
  if (SV_LANGUAGES.has(lang.toLowerCase())) return SV_GRAMMAR;
  if (isCLanguage(lang)) return C_GRAMMAR;
  return PLAIN_GRAMMAR;
}

const WORD = /[A-Za-z_]/;
const WORDCH = /\w/;
const OP = new Set("=+-*/%<>!&|^~?:".split(""));

function scanLine(
  line: string,
  inBlock: boolean,
  g: Grammar,
): { tokens: Token[]; inBlock: boolean } {
  const tokens: Token[] = [];
  let i = 0;
  let plainStart = 0;
  const flushPlain = (end: number) => {
    if (end > plainStart) tokens.push({ text: line.slice(plainStart, end), cls: "plain" });
  };

  while (i < line.length) {
    if (inBlock) {
      const close = line.indexOf("*/", i);
      if (close === -1) {
        tokens.push({ text: line.slice(i), cls: "comment" });
        i = line.length;
      } else {
        tokens.push({ text: line.slice(i, close + 2), cls: "comment" });
        i = close + 2;
        inBlock = false;
      }
      plainStart = i;
      continue;
    }

    const c = line[i];
    const next = line[i + 1] ?? "";

    if (c === "/" && next === "/") {
      flushPlain(i);
      tokens.push({ text: line.slice(i), cls: "comment" });
      i = line.length;
      plainStart = i;
      break;
    }
    if (c === "/" && next === "*") {
      flushPlain(i);
      const close = line.indexOf("*/", i + 2);
      if (close === -1) {
        tokens.push({ text: line.slice(i), cls: "comment" });
        i = line.length;
        inBlock = true;
      } else {
        tokens.push({ text: line.slice(i, close + 2), cls: "comment" });
        i = close + 2;
      }
      plainStart = i;
      continue;
    }
    if (c === '"') {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length) {
        if (line[j] === "\\") {
          j += 2;
          continue;
        }
        if (line[j] === '"') {
          j++;
          break;
        }
        j++;
      }
      tokens.push({ text: line.slice(i, j), cls: "string" });
      i = j;
      plainStart = i;
      continue;
    }
    // `define (SystemVerilog) / #include (C preprocessor), per grammar.
    if (g.directiveSigil && c === g.directiveSigil && WORD.test(next)) {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length && WORDCH.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), cls: "directive" });
      i = j;
      plainStart = i;
      continue;
    }
    if (g.systask && c === "$" && WORD.test(next)) {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length && WORDCH.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), cls: "systask" });
      i = j;
      plainStart = i;
      continue;
    }
    // C/C++ char literal ('a', '\n'). Checked before the numeric branch, which in
    // SystemVerilog is what a leading ' means instead.
    if (g.charLiteral && c === "'") {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length) {
        if (line[j] === "\\") {
          j += 2;
          continue;
        }
        if (line[j] === "'") {
          j++;
          break;
        }
        j++;
      }
      tokens.push({ text: line.slice(i, j), cls: "string" });
      i = j;
      plainStart = i;
      continue;
    }
    if (/[0-9]/.test(c) || (g.tickNumber && c === "'" && /[sSbBoOdDhH]/.test(next))) {
      const m = g.num.exec(line.slice(i));
      if (m) {
        flushPlain(i);
        tokens.push({ text: m[0], cls: "number" });
        i += m[0].length;
        plainStart = i;
        continue;
      }
    }
    if (WORD.test(c)) {
      let j = i + 1;
      while (j < line.length && WORDCH.test(line[j])) j++;
      const word = line.slice(i, j);
      const cls: TokenClass | null = g.types.has(word)
        ? "type"
        : g.keywords.has(word)
          ? "keyword"
          : null;
      if (cls) {
        flushPlain(i);
        tokens.push({ text: word, cls });
        plainStart = j;
      }
      i = j; // a plain identifier stays inside the surrounding plain run
      continue;
    }
    if (OP.has(c)) {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length && OP.has(line[j])) j++;
      tokens.push({ text: line.slice(i, j), cls: "operator" });
      i = j;
      plainStart = i;
      continue;
    }
    i++; // whitespace / punctuation ⇒ part of the plain run
  }
  flushPlain(line.length);
  return { tokens, inBlock };
}

// Tokenize a whole source file into one `Token[]` per line. `lang` is the file's
// `SourceFile.language` (omitted/null ⇒ SystemVerilog).
export function tokenizeLines(text: string, lang?: string | null): Token[][] {
  const g = grammarFor(lang);
  const lines = text.split(/\r\n|\r|\n/);
  const out: Token[][] = [];
  let inBlock = false;
  for (const line of lines) {
    const res = scanLine(line, inBlock, g);
    out.push(res.tokens);
    inBlock = res.inBlock;
  }
  return out;
}

// Lexical syntax tokenizer for the source panes (#223). DOM-free + unit-tested (like
// log.ts / csrc.ts / schempick.ts); main.ts renders the tokens. Scope is lexical only —
// identifiers stay `plain`; model-driven name coloring is #225. A whole-file scan carries
// `/* */` block-comment state across line boundaries and returns one token row per line.
//
// Invariant: for every line, concatenating its tokens' `text` reproduces the line exactly.
// `sourceOffsetAt` (via srcoffset.ts `lineColumn`) relies on it to keep the byte-offset
// cross-probe correct now that a line renders as many nodes instead of one text node.

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

const SV_GRAMMAR: Grammar = { keywords: SV_KEYWORDS, types: SV_TYPES };
// A language with no registered grammar still lexes comments/strings/numbers/operators
// (correct for C/C++ too) but claims no keywords — #224 registers the real C/C++ grammar.
const PLAIN_GRAMMAR: Grammar = { keywords: new Set<string>(), types: new Set<string>() };

const GRAMMARS: Record<string, Grammar> = { systemverilog: SV_GRAMMAR };

function grammarFor(lang: string | null | undefined): Grammar {
  if (lang == null) return SV_GRAMMAR; // absent language ⇒ SystemVerilog (SourceFile convention)
  return GRAMMARS[lang.toLowerCase()] ?? PLAIN_GRAMMAR;
}

const WORD = /[A-Za-z_]/;
const WORDCH = /\w/;
const OP = new Set("=+-*/%<>!&|^~?:".split(""));
// Sized literal (32'hFF, 'd5) is tried before a plain decimal so 32'hFF isn't split.
const NUM = /^(?:\d[\d_]*)?'[sS]?[bBoOdDhH][0-9a-fA-FxXzZ_]+|^\d[\d_]*(?:\.\d[\d_]*)?/;

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
    if (c === "`" && WORD.test(next)) {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length && WORDCH.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), cls: "directive" });
      i = j;
      plainStart = i;
      continue;
    }
    if (c === "$" && WORD.test(next)) {
      flushPlain(i);
      let j = i + 1;
      while (j < line.length && WORDCH.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), cls: "systask" });
      i = j;
      plainStart = i;
      continue;
    }
    if (/[0-9]/.test(c) || (c === "'" && /[sSbBoOdDhH]/.test(next))) {
      const m = NUM.exec(line.slice(i));
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

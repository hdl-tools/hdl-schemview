// @vitest-environment happy-dom
import { describe, it, expect } from "vitest";
import { lineColumn } from "./srcoffset";

function makeLine(gutter: string): HTMLElement {
  const div = document.createElement("div");
  div.className = "line";
  div.innerHTML = `<span class="ln">${gutter}</span>`;
  return div;
}
function span(cls: string, text: string): HTMLSpanElement {
  const s = document.createElement("span");
  s.className = cls;
  s.textContent = text;
  return s;
}

describe("lineColumn", () => {
  it("returns the caret offset for a single plain text token (gutter skipped)", () => {
    const div = makeLine("1");
    const text = document.createTextNode("assign x = y;");
    div.appendChild(text);
    expect(lineColumn(div, text, 7)).toBe(7);
  });

  it("sums preceding token nodes before the caret's plain token", () => {
    const div = makeLine("2");
    div.appendChild(document.createTextNode("  "));
    div.appendChild(span("tok-keyword", "assign"));
    const mid = document.createTextNode(" x = ");
    div.appendChild(mid);
    expect(lineColumn(div, mid, 1)).toBe(2 + 6 + 1); // "  " + "assign" + 1
  });

  it("counts a caret inside a highlighted token span", () => {
    const div = makeLine("3");
    div.appendChild(document.createTextNode("x = "));
    const num = span("tok-number", "");
    const numText = document.createTextNode("32'hFF");
    num.appendChild(numText);
    div.appendChild(num);
    expect(lineColumn(div, numText, 3)).toBe(4 + 3); // "x = " + 3 into the number
  });

  it("matches the plain-text column across a fully tokenized line", () => {
    // "  assign y = 32'hFF;" rendered as tokens; the caret sits on the ';'.
    const div = makeLine("4");
    div.appendChild(document.createTextNode("  "));
    div.appendChild(span("tok-keyword", "assign"));
    div.appendChild(document.createTextNode(" y "));
    div.appendChild(span("tok-operator", "="));
    div.appendChild(document.createTextNode(" "));
    div.appendChild(span("tok-number", "32'hFF"));
    const tail = document.createTextNode(";");
    div.appendChild(tail);
    const plain = "  assign y = 32'hFF;";
    expect(lineColumn(div, tail, 0)).toBe(plain.indexOf(";"));
  });

  it("returns 0 at the very start of the line's code", () => {
    const div = makeLine("5");
    const kw = span("tok-keyword", "module");
    const kwText = kw.firstChild as Node;
    div.appendChild(kw);
    expect(lineColumn(div, kwText, 0)).toBe(0);
  });
});

// Pure logic for the C/C++ source pane (#159, HLS C↔RTL tracing). DOM-free like
// log.ts / prefs.ts / schempick.ts; main.ts owns the pane wiring. A source file's
// `language` decides which pane renders it: the RTL "Source" tab (systemverilog or
// untagged) or the "C/C++ source" tab.

import type { SourceFile } from "./types";

/** C/C++ source-language tags emitted by the harness (#159). */
const C_LANGUAGES = new Set(["c", "cpp", "cc", "cxx", "h", "hpp"]);

/**
 * Whether a file `language` tag denotes a C/C++ high-level source (as opposed to
 * SystemVerilog RTL). Untagged / `"systemverilog"` ⇒ false, so a non-HLS design
 * (no `language` set anywhere) never reveals the C pane.
 */
export function isCLanguage(language: string | null | undefined): boolean {
  return language != null && C_LANGUAGES.has(language.toLowerCase());
}

/** The C/C++ source files among a design's files, in table order (#159). */
export function cSourceFiles(files: SourceFile[]): SourceFile[] {
  return files.filter((f) => isCLanguage(f.language));
}

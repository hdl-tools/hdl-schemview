# Architecture decision records

One file per decision with lasting consequences. This index is the single list — if you
add an ADR, add its row here rather than starting a second list elsewhere.

| # | Decision | In one line |
| --- | --- | --- |
| [0001](0001-scope-rtl-vs-netlist.md) | RTL vs netlist scope | The tool targets **RTL-level** elaboration, not gate netlists |
| [0002](0002-distribution-open-vs-fsdb.md) | Open vs FSDB distribution | Ship open formats only; proprietary readers load out-of-process as user plugins |
| [0003](0003-storage-backend-for-parse-scalability.md) | Storage backend for parse scalability | Phased: rkyv load cache first (Option A, owned deserialize), redb/SQLite only if measurement demands it |
| [0004](0004-internal-logic-schematic-granularity.md) | Internal-logic granularity | Drill a leaf module to **process** granularity — one box per `always`/`assign` |
| [0005](0005-optional-gate-level-projection.md) | Optional gate-level projection | Gate/mux decomposition is an **opt-in projection** of the same model, additive and off by default |
| [0006](0006-hls-cpp-rtl-source-tracing.md) | HLS C↔RTL source tracing | Use the generating tool's **own provenance comments**; never infer the correspondence, and never parse C |
| [0007](0007-model-driven-semantic-name-coloring.md) | Model-driven semantic name coloring | Identifier colors come from what the **elaboration resolved**, not from the token text |
| [0008](0008-lexical-source-highlighting.md) | Lexical source highlighting | Keywords/comments/strings are lexed; the lexer never guesses at identifiers |
| [0009](0009-packaging-for-isolated-environments.md) | Packaging for isolated environments | The bundle carries its own runtime; tiers by network availability |
| [0010](0010-schematic-trace-mode.md) | Schematic trace mode | Tracing is a **seeded, boundary-crossing projection** with visible level-of-detail caps |
| [0011](0011-rkyv-cache-validation-policy.md) | rkyv cache validation policy | **Proposed:** gate unchecked access behind a cheap checksum, with the JSON as fallback — bytecheck costs 294 ms at 1M |

## Writing a new one

Number sequentially, name the file `NNNN-kebab-summary.md`, and follow the shape the
existing records use: context, the decision, the alternatives considered, and the
consequences. Record the decision *and* what it costs — an ADR whose consequences section
is empty is a note, not a decision.

Keep the status line current. Several of these have been amended by later measurement
(0003 especially — see [benchmarking.md](../benchmarking.md)), and saying so in the record
is more useful than a clean-looking file.

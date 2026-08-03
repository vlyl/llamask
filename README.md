# LlaMask

**English** | [简体中文](README.zh-CN.md)

[![CI](https://github.com/vlyl/llamask/actions/workflows/ci.yml/badge.svg)](https://github.com/vlyl/llamask/actions/workflows/ci.yml)

> Current release: `v0.1.0-alpha.1`, a developer preview for validating the
> redaction core, document adapters, and desktop shell. It does not yet include
> signed installers for end users or ready-to-use model weights.

LlaMask is a local, offline data-redaction tool for individuals and
organizations. It detects sensitive information in text, Office documents,
PDFs, and images without uploading files or relying on cloud services, lets the
user review the findings, and produces a redacted copy.

The first model evaluation is complete and the project has entered runnable
prototype development. End-to-end slices now work for text, images, DOCX,
XLSX, PPTX, and PDF: UTF-8 TXT/Markdown/standard input, PNG/JPEG, visible and
hidden Office text, notes, comments, masters, and embedded images can be
scanned with rules and local models. PDFs support page-level OCR, editable
masks, and safe full-page rasterized export. Every format produces an editable
task draft and a safe copy, followed by an independent residual-data rescan
before the output is committed to disk.

## Core principles

- Fully offline: once installed, LlaMask works without an internet connection
  and makes no network requests while running.
- Source-file safety: source files are never overwritten; only new copies are
  created.
- Recall first: flagging extra suspicious content is preferable to silently
  missing sensitive data.
- Human in the loop: high-confidence findings can be handled automatically,
  while ambiguous findings remain available for review.
- Explainable results: every finding should report its type, origin, and reason.
- Local adaptation: dictionaries, rules, allowlists, and user feedback adapt to
  local habits without sending data elsewhere.
- Format fidelity: preserve layout, formulas, styles, and editability whenever
  this can be done safely.

## Design documents

The design documents are currently written in Chinese:

- [Product requirements](docs/01-product-requirements.md)
- [Technical architecture](docs/02-technical-architecture.md)
- [MVP roadmap](docs/03-mvp-roadmap.md)
- [Product decision record](docs/04-product-decisions.md)
- [Policy configuration model](docs/05-policy-model.md)
- [Local model selection report](docs/06-model-selection.md)
- [Model benchmark and first-release freeze](docs/07-model-benchmark-results.md)
- [Policy and local-model sidecar protocol](docs/08-policy-and-sidecar-protocol.md)
- [DOCX vertical slice and safety boundaries](docs/09-docx-vertical-slice.md)
- [XLSX vertical slice and safety boundaries](docs/10-xlsx-vertical-slice.md)
- [PPTX vertical slice and safety boundaries](docs/11-pptx-vertical-slice.md)
- [PDF vertical slice and safety boundaries](docs/12-pdf-vertical-slice.md)
- [Desktop MVP architecture and milestones](docs/13-desktop-mvp-architecture.md)
- [Offline desktop runtime packaging contract](docs/14-offline-runtime-packaging.md)

## Target scope for the first release

- A fully offline desktop application for both individuals and organizations.
- Windows and macOS support.
- TXT, Markdown, clipboard text, DOCX, XLSX, PPTX, PDF, and common image formats.
- Detection combining deterministic rules, sensitive-term dictionaries,
  validation algorithms, Chinese information extraction, OCR/layout models,
  and a local 4B multimodal language model.
- Automatic policy application with an editable redaction draft that users can
  undo, modify, or extend.
- Policy-controlled batch export without mandatory review, with unsafe files
  blocked individually.
- Configurable redaction methods, automatic-processing thresholds, and
  sensitive-information categories.
- Fidelity-first PDF handling, with safe rasterization of affected pages when
  secure object-level removal cannot be guaranteed.
- Configurable consistent replacement scope and reversible recovery; the
  default is task-scoped consistency without recovery.
- A complete scan, review, safe-copy generation, and independent verification
  loop.
- User feedback remains on the local machine. The MVP does not train model
  weights.

See the product requirements document for the complete scope and acceptance
criteria.

## Runnable prototype

Rust 1.97 or later is required. From the repository root, run:

```bash
cargo run -p llamask -- scan sample.txt --task task.json
cargo run -p llamask -- export task.json --output sample_redacted.txt \
  --runtimes config/runtimes/development-siamese.json
cargo run -p llamask -- verify task.json sample_redacted.txt \
  --runtimes config/runtimes/development-siamese.json
```

`task.json` is an editable review draft. `selected` controls whether a finding
is redacted, while `replacement` controls its replacement value. Low-confidence
model findings default to `reviewed: false`; the user must set `reviewed` to
`true` after confirming either redaction or retention, otherwise export is
blocked. The prototype never overwrites the source or an existing output file,
and it rejects export if the source changes after scanning.

Policies also support exact sensitive terms and allowlists:

```json
{
  "exact_terms": [
    { "value": "Project Starsea", "entity_type": "PROJECT_CODE" }
  ],
  "allowlist": ["public@example.com"]
}
```

Add these fields to a complete policy JSON document. Exact terms use literal
source-text matching; allowlist entries suppress only findings whose entire
matched value is identical.

The clipboard-compatible flow builds a task from standard input and writes
independently rescanned redacted text to standard output:

```bash
cargo run -p llamask -- scan-stdin --task clipboard-task.json
cargo run -p llamask -- render clipboard-task.json
```

The CLI does not monitor or read the system clipboard in the background. The
desktop UI will invoke the same entry points only after an explicit user action.

### PNG and JPEG

The image workflow uses local PP-OCRv6 small. Rule-only mode requires about
31 MB of OCR weights; the lightweight AI combination also loads SiameseUIE for
person and organization recognition. In an image task, `mask_rect` is the final
pixel rectangle and can be edited manually. `selected` and `reviewed` control
redaction and review state:

```bash
cargo run -p llamask -- scan-image sample.png \
  --task image-task.json \
  --runtimes config/runtimes/development-image-small.json

cargo run -p llamask -- export-image image-task.json \
  --output sample_redacted.png \
  --runtimes config/runtimes/development-image-small.json

cargo run -p llamask -- verify-image image-task.json sample_redacted.png \
  --runtimes config/runtimes/development-image-small.json
```

The core estimates matched-fragment rectangles from OCR text lines and adds the
policy's safety margin. Numbers split across visual lines share a `group_id` and
produce multiple rectangles. Rotated text, or text that cannot be segmented
reliably, falls back to masking the whole line. Export uses solid masks: PNG is
re-encoded losslessly with transparency preserved, while JPEG uses quality 95.
Phone-photo EXIF orientation is baked into the pixels before source metadata is
removed. A candidate copy must pass another OCR and rule/optional-model rescan
before it is committed atomically. Output must use the same format as the input
and cannot overwrite the source or an existing file.

### DOCX

The DOCX workflow scans body text, tables, headers and footers, footnotes,
endnotes, comments, chart text, deleted revisions, and field codes. A finding
split across multiple Word runs is detected as one value, while only the
intersecting text nodes are changed. Unmatched XML and media entries are not
rebuilt from plain text. Export also removes authorship, comment/revision
identities, document variables, custom properties, external hyperlink targets,
thumbnails, ZIP comments, and timestamps:

```bash
cargo run -p llamask -- scan-docx sample.docx \
  --task docx-task.json

cargo run -p llamask -- export-docx docx-task.json \
  --output sample_redacted.docx

cargo run -p llamask -- verify-docx docx-task.json sample_redacted.docx
```

Embedded PNG/JPEG images in DOCX files are recursively connected to the image
pipeline. When scanning with an OCR runtime registry, the task draft stores an
editable mask subtask for each image in `embedded_images`. During export, each
image is re-encoded without metadata, replaced under `word/media`, and scanned
again inside the candidate DOCX:

```bash
cargo run -p llamask -- scan-docx sample.docx \
  --task docx-task.json \
  --runtimes config/runtimes/development-ocr-small.json
cargo run -p llamask -- export-docx docx-task.json \
  --output sample_redacted.docx \
  --runtimes config/runtimes/development-ocr-small.json
```

Without an OCR runtime, text findings are still available, but export of a
document containing images fails closed and requires a new scan. Unsupported
media such as GIF, TIFF, and SVG, as well as macros, ActiveX, OLE, and embedded
objects, also block export. Verification reports never repeat residual source
values. `complete: false` means that not every optional local model enabled by
the policy participated in the rescan.

### XLSX

The XLSX workflow covers ordinary, shared, and inline strings; numeric cells;
formulas and cached results; legacy comments; headers and footers; hidden rows,
columns, and sheets; defined names; DrawingML text; and embedded PNG/JPEG
images. Formula findings require review by default. Once redaction is confirmed,
the formula is removed and the entire cell becomes plain text so a sensitive
literal cannot remain in a formula or cache:

```bash
cargo run -p llamask -- scan-xlsx sample.xlsx \
  --task xlsx-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-xlsx xlsx-task.json \
  --output sample_redacted.xlsx \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-xlsx xlsx-task.json sample_redacted.xlsx \
  --runtimes config/runtimes/development-ocr-small.json
```

A selected shared-string cell is converted into an inline string while keeping
its style, and unreferenced shared-string originals are cleared. Comment authors
and Office/ZIP privacy metadata are also removed. Sheet names are not renamed
automatically; a finding there must be explicitly reviewed for retention.
Charts, pivot caches, external data connections, macros, ActiveX, embedded
objects, and non-PNG/JPEG media currently fail closed so the tool never emits an
apparently successful file that may still contain hidden data. See the XLSX
safety-boundary document for details.

### PPTX

The PPTX workflow covers slide text boxes, tables, grouped shapes, text split
across runs, speaker notes, modern comments and replies, hidden slides, slide
masters, layouts, SmartArt text, and embedded PNG/JPEG images. Replacements are
written directly into intersecting `a:t` nodes, preserving the slide structure
and run styles:

```bash
cargo run -p llamask -- scan-pptx sample.pptx \
  --task pptx-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-pptx pptx-task.json \
  --output sample_redacted.pptx \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-pptx pptx-task.json sample_redacted.pptx \
  --runtimes config/runtimes/development-ocr-small.json
```

Export neutralizes comment authors and timestamps, removes drawing descriptions
and custom data, and rewrites external hyperlinks to `about:blank`. Charts and
caches, embedded workbooks, external data, macros, ActiveX, 3D models, audio,
video, and non-PNG/JPEG media currently fail closed. See the PPTX
safety-boundary document for details.

### PDF

The first PDF safety baseline renders every page with Poppler at a fixed 200 DPI
and then reuses the image OCR and editable `mask_rect` task model. Export does
not copy the source PDF object tree. Instead, it constructs a new PDF from the
masked pages as JPEG images, so forms, annotations, links, attachments, scripts,
hidden text, metadata, and incremental history do not enter the copy:

```bash
cargo run -p llamask -- scan-pdf sample.pdf \
  --task pdf-task.json \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- export-pdf pdf-task.json \
  --output sample_redacted.pdf \
  --runtimes config/runtimes/development-ocr-small.json

cargo run -p llamask -- verify-pdf pdf-task.json sample_redacted.pdf \
  --runtimes config/runtimes/development-ocr-small.json
```

This mode preserves the page appearance and order but not text search/copy,
vector editing, forms, or link interaction. Output must pass another page-level
OCR scan, target-value check, and strict image-only PDF structure validation
before it is committed atomically. The host or installer must currently provide
`pdfinfo` and `pdftoppm`; `LLAMASK_PDFINFO` and `LLAMASK_PDFTOPPM` can point to
bundled copies. Encrypted PDFs, files larger than 100 MiB, documents longer than
200 pages, or documents exceeding 500 million rendered pixels fail closed. See
the PDF safety-boundary document for details.

### Policies and local models

Generate and validate policies with:

```bash
cargo run -p llamask -- policy init my-policy.json
cargo run -p llamask -- policy validate my-policy.json
cargo run -p llamask -- runtimes verify config/runtimes/development-siamese.json
```

Use `--policy` to choose a policy during scanning and `--runtimes` to choose a
local-model runtime registry. If an optional model is missing or fails, the task
draft records an explicit degradation warning. If the policy marks a model as
`required`, scanning is blocked instead. Passing the same runtime registry to
export and verification runs the rules and models again after replacement. The
report's `complete` field indicates whether every enabled model participated in
the rescan; the report never echoes residual source text.

A real text-model combination can be invoked with:

```bash
target/release/llamask scan sample.txt \
  --task task.json \
  --policy config/policies/default.json \
  --runtimes config/runtimes/development-text-models-cpu.json
```

Task drafts currently contain source text, OCR text, and matched values and must
therefore be handled as sensitive files. Development sidecars exist for real
SiameseUIE and Qwen Q4 models. The default SiameseUIE path now uses pure ONNX
and no longer depends on PyTorch, Transformers, or ModelScope. Qwen's
single-record path still shows semantic drift relative to the frozen batch
evaluation, so both models currently produce uncalibrated findings that require
review. The lightweight image combination currently includes OCR, rules, and
SiameseUIE; Qwen will join the default image pipeline only after a persistent
model process is implemented. Mock sidecars in this repository are for protocol
testing only.

The graphical workflow and a PDF mode that preserves objects or adds a clean
search layer remain future slices. The next release gate for the Office and PDF
adapters is real-file regression testing on both platforms with Microsoft
Office, LibreOffice, Keynote, and native PDF readers, plus expanded safe media
support beyond PNG/JPEG.

## Desktop status

Desktop development lives under `apps/llamask-desktop`. It provides a minimal
Tauri 2 + React/TypeScript window, restricted file selection, drag and drop
import, a Rust session-path registry, format and size preflight checks, and a
task-list interface. TXT, Markdown, DOCX, XLSX, PPTX, PDF, PNG, and JPEG use the
real `llamask-core` scanner on a controlled background worker with path-free
progress events and cooperative cancellation. Sensitive scan drafts remain in
Rust memory. Image/PDF review requests only a bounded, re-encoded page preview and
geometry. Text review requests only the matched value and 80 Unicode characters
of context on each side, never the source path or whole document. DOCX findings
also include only a safe section label such as body, header, or comment; OOXML
locators stay inside the Rust task. Embedded PNG/JPEG images can be reviewed in
the same editable mask workspace after their source package hash is revalidated.
Clipboard text can now be imported through an explicit user action, scanned and reviewed
with the same bounded text workflow, and copied back only after fail-closed
residual verification succeeds. Clipboard source text remains in Rust session
memory and is never sent in progress events or written to application storage.
Users can edit text replacements, retain findings, move and resize masks, add
manual masks, and invoke fail-closed safe export through a native save dialog.
DOCX export remains disabled until both text findings and embedded-image groups
have been reviewed, and it reuses the Core package rewrite and residual checks.
XLSX review exposes only safe worksheet ordinals, cell references, and content
types. Formula and cache findings for the same cell are reviewed atomically;
sheet-name findings can only be explicitly retained. Embedded images share the
same hash-validated mask workflow, and export reuses the Core workbook rewrite
and residual checks. PPTX review exposes only safe slide/story ordinals and
bounded context for slides, notes, comments, masters, layouts, and diagrams.
Embedded images share the same hash-validated mask workflow; unsupported charts,
external data, embedded objects, active content, and media fail closed before
review. PPTX export reuses the Core package rewrite and independent residual
checks. Batch export now selects one native output directory, processes every
auto-confirmed or reviewed file sequentially, avoids name collisions, isolates
per-file failures, and emits only aggregate completion counts. Unreviewed files
are skipped. The offline runtime packaging contract now pins OCR executables,
models, `pdfinfo`, `pdftoppm`, and dependent files by SHA-256, prepares a
path-safe Tauri resource directory from platform recipes, and prevents declared
tools from falling back after integrity failure. Signed Apple Silicon and
Windows x64 payloads, license manifests, and real-machine regression remain the
next release gate.
See the desktop architecture document for development details and safety
boundaries.

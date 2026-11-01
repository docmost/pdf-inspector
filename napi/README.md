# @docmost/pdf-inspector

Fast PDF classification, text extraction, and **image extraction** for Node.js/Bun. Native Rust performance via [napi-rs](https://napi.rs).

This is a fork of [`@docmost/pdf-inspector`](https://github.com/firecrawl/pdf-inspector). **This fork adds:**

- `extractImages(buffer)` — extract embedded JPEG/PNG images as raw `Buffer` data with page position
- `processPdfWithImages(buffer)` — markdown + images in one call, with `pdf-image://N` placeholders matching the `images` array
- Extra build targets: `linux-arm64-gnu` and `win32-x64-msvc`

> Originally built by [Firecrawl](https://firecrawl.dev) for hybrid OCR pipelines.

## Features

- **Smart classification** — text-based / scanned / image-based / mixed in ~10–50ms, with a confidence score and per-page OCR routing.
- **Region-based extraction** — pull text from bounding boxes with per-region quality checks (`needsOcr`).
- **Layout-aware** — multi-column reading order, position and font info per text item, RTL support.
- **Robust text decoding** — CID/Type0 fonts via ToUnicode CMaps, plus automatic flagging of broken encodings so callers can fall back to OCR.
- **Selective OCR** — `Auto` routes only pages rejected by native extraction and returns source/model provenance plus hosted-fallback recommendations.
- **External artifacts** — the native package embeds no OCR models, PDFium, or ONNX Runtime; clean `Auto` requests never load or download them.

## Benchmark

[opendataloader-bench](https://github.com/opendataloader-project/opendataloader-bench) corpus (200 PDFs), local engines without model-based PDF parsing; OCR disabled. Scores 0–1, higher is better:

| Engine | Overall | Reading order | Tables (TEDS) | Headings | Speed |
|---|---|---|---|---|---|
| **pdf-inspector** | **0.875** | **0.915** | **0.814** | 0.788 | **0.470s** |
| liteparse | 0.873 | 0.913 | 0.693 | **0.811** | 0.750s |
| opendataloader | 0.831 | 0.902 | 0.489 | 0.739 | 2.569s |
| pymupdf4llm | 0.735 | 0.886 | 0.401 | 0.424 | 17.117s |
| markitdown | 0.589 | 0.844 | 0.273 | 0.000 | 16.165s |

Refreshed July 31, 2026, on Apple M4 Pro; speed is the median of five complete corpus runs after an excluded warm-up. Full methodology and versions are in the [repo README](https://github.com/firecrawl/pdf-inspector#benchmark), with raw timings and artifacts in the [results branch](https://github.com/firecrawl/opendataloader-bench/tree/abi/pdf-parser-benchmark-results).

## Install

```bash
npm install @docmost/pdf-inspector
# or
bun add @docmost/pdf-inspector
```

Prebuilt binaries included for **linux-x64**, **linux-arm64**, **macOS ARM64**, and **windows-x64**. No Rust toolchain needed.

## API

### `processPdf(buffer: Buffer, pages?: number[]): PdfResult`

Full PDF processing — classify, extract text, and convert to Markdown in one call.

```typescript
import { processPdf } from '@docmost/pdf-inspector'
import { readFileSync } from 'fs'

const pdf = readFileSync('document.pdf')
const result = processPdf(pdf)

console.log(result.pdfType)    // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
console.log(result.markdown)   // Markdown string or null
console.log(result.pageCount)  // 42
```

### `classifyPdf(buffer: Buffer): PdfClassification`

Classify a PDF as TextBased, Scanned, Mixed, or ImageBased (~10-50ms). Returns which pages need OCR.

```typescript
import { classifyPdf } from '@docmost/pdf-inspector'

const result = classifyPdf(readFileSync('document.pdf'))

console.log(result.pdfType)         // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
console.log(result.pageCount)       // 42
console.log(result.pagesNeedingOcr) // [5, 12, 15] (0-indexed)
console.log(result.confidence)      // 0.875
```

### `processPdfWithImages(buffer: Buffer, pages?: number[]): PdfResultWithImages`

Process a PDF and extract both markdown and images in one call. The markdown contains `![image](pdf-image://N)` placeholders where `N` is the index into the returned `images` array.

```typescript
import { processPdfWithImages } from '@docmost/pdf-inspector'
import { readFileSync } from 'fs'

const result = processPdfWithImages(readFileSync('document.pdf'))

console.log(result.images.length)  // 110
console.log(result.markdown)       // "# Title\n\n![image](pdf-image://0)\n\nSome text..."

// Replace placeholders with real URLs after uploading
let content = result.markdown
for (let i = 0; i < result.images.length; i++) {
  const img = result.images[i]
  const ext = img.format === 'Jpeg' ? 'jpg' : 'png'
  const url = await uploadImage(img.data, `image-${i}.${ext}`)
  content = content.replace(`pdf-image://${i}`, url)
}
```

### `extractImages(buffer: Buffer): ExtractedImage[]`

Extract embedded images from a PDF as raw bytes. Supports JPEG (DCTDecode) and PNG (FlateDecode) images. Returns image data with page position and dimensions.

Image extraction is a separate function from text extraction — calling `processPdf` or `extractText` does **not** pay the cost of image decompression/encoding.

```typescript
import { extractImages } from '@docmost/pdf-inspector'
import { writeFileSync } from 'fs'

const images = extractImages(readFileSync('document.pdf'))

for (const img of images) {
  const ext = img.format === 'Jpeg' ? 'jpg' : 'png'
  writeFileSync(`page${img.page}_${img.width}x${img.height}.${ext}`, img.data)
  console.log(`Page ${img.page}: ${img.width}x${img.height} ${img.format}`)
}
```

### `extractTextWithPositions(buffer: Buffer, pages?: number[], options?: FrameOptions): TextItem[]`

An async variant, `extractTextWithPositionsAsync`, takes the same arguments and
resolves to the same items (see [Async variants](#async-variants)).

Every text item (plus image placeholders, links and form fields) with its font
and position. `x`/`y` are PDF points relative to the page's **visible page
box** (`CropBox ∩ MediaBox`, else the MediaBox), origin at the box's lower-left
corner with `y` growing upward. `extractTextInRegions` reads its regions
relative to the same box but from its top-left corner with `y` growing
downward, so flip with the box height: `boxHeight - y`. For text items `y` is
the baseline and `height` the font size, so
`[x, boxHeight - y - height, x + width, boxHeight - y]` covers the glyph band
above the baseline (descenders fall below it); for image, link and form-field
items `y` is the rect bottom and that box is exact. Pages whose CropBox equals
the MediaBox at `(0, 0)` are unaffected.

By default the page `/Rotate` is not applied and a page whose text is
predominantly rotated is turned so that text reads left-to-right (this is the
`"sheet"` frame; `extractTextWithPositionsAndRotations` reports which pages
were turned). Pass `{ frame: "display" }` to get every item in the rendered
page's frame instead — the visible page box turned clockwise by the page's
inheritable `/Rotate`, lower-left origin, `y` up, with the turn of a rotated
page undone — so `x`/`y`/`width`/`height` and `rotation` describe the item as
a renderer draws it. Pages with `/Rotate 0` whose text is not predominantly
rotated are identical in both frames.

`fontWeight` is the font's weight class on the 100..900 scale shared by CSS
`font-weight` and the OS/2 `usWeightClass` field (400 regular, 700 bold), read
from the embedded font program's OS/2 table, else the FontDescriptor's
`/FontWeight`, else a weight word in the font name ("Light", "Medium", "-Md",
"Black", "W6"); it is omitted when none of them says. `isBold` is unchanged
and independent of it, so a medium face reports `fontWeight: 500` with
`isBold: false`. Pass `{ boldFromWeight: true }` to also read `isBold` from a
weight class of 600 (SemiBold) or more — or of `boldWeightThreshold`, any
class on the 100..900 scale — and to merge adjacent runs by that verdict, so a
heavier run inside a lighter paragraph keeps its own item while runs whose
weights differ but agree on bold merge as usual.

`boldSource` says where `isBold` came from — `"FontName"` (a bold word or
style abbreviation in the font name), `"FontFlags"` (the FontDescriptor's
ForceBold flag or the embedded program's bold selection), `"WeightClass"`
(the weight class, with `boldFromWeight`) or `"Painted"` (text filled and
stroked to look heavier), the first of them in that order when more than one
says bold — so a face whose name says Bold over a `fontWeight` of 400 can be
told from one whose weight class says so. It is omitted when `isBold` is
`false`.

`fixedPitch` is `true` when the FontDescriptor's FixedPitch flag or the
embedded program's `post` table says the font is monospaced, else measured
from the font's width table: `true` when a dozen or more of its glyphs share
one advance, `false` when two differ. It is omitted when the font declares
nothing and no two advances differ but fewer than a dozen share one. Many
producers write `/Flags 4`
whatever the face, so the flag is only ever read as a yes.

`fillColor` and `strokeColor` are the colours a run was shown with, as sRGB
`[red, green, blue]` arrays of 0..255: its glyphs are filled with the first in
render modes 0, 2, 4 and 6 and outlined with the second in modes 1, 2, 5 and
6. DeviceRGB is read as sRGB, DeviceGray as three equal components and
DeviceCMYK converted the way the PDF specification converts it to DeviceRGB;
ICCBased spaces are read by their component count and Indexed spaces through
their palette. A colour is omitted for any other colour space (Separation,
DeviceN, Pattern, CalRGB, Lab, ...). `renderMode` is the text render mode
(`Tr`) the run was shown with, 0..7: runs in mode 3 (invisible, the mode of OCR
text layers) and mode 7 (clipping only) put no glyphs on the page, so a caller
can tell visible text from invisible text. The colours and the mode are
graphics state — they hold across text objects, `q`/`Q` save and restore them,
and a Form XObject starts with the paint it was invoked under — and reporting
them changes nothing about which runs are extracted. All three are omitted for
image, link and form-field items, and a merged item keeps its first run's.

`legacySymbolRewrite: true` marks items whose decoded text includes a character
changed by legacy symbol cleanup. Merged items retain this evidence from either
source, and split items conservatively inherit it. The field is omitted when
that cleanup did not change a character; absence is not a general guarantee of
decoding accuracy. Consumers correcting other text can use the marker to avoid
treating a rewritten symbol as an authoritative Unicode value.

```typescript
import { extractTextWithPositions } from '@docmost/pdf-inspector'

for (const item of extractTextWithPositions(pdf, [1])) { // pages are 1-indexed
  console.log(item.page, item.text, item.x, item.y, item.fontSize)
}

// Boxes as a renderer draws the page (`/Rotate` applied)
const rendered = extractTextWithPositions(pdf, undefined, { frame: 'display' })

// Bold also from the weight class, SemiBold (600) and heavier
const weighted = extractTextWithPositions(pdf, undefined, { boldFromWeight: true })

// ... or from Bold (700) and heavier
const heavier = extractTextWithPositions(pdf, undefined, { boldFromWeight: true, boldWeightThreshold: 700 })
```

### `extractTextWithPositionsAndRotations(buffer: Buffer, pages?: number[], options?: FrameOptions): PositionedText`

`extractTextWithPositions` plus `pageRotations`, one `{ page, rotation: 'ccw' | 'cw' }`
entry per page whose text was predominantly rotated and therefore turned in the
`"sheet"` frame. With `{ frame: "display" }` the items are in the rendered
page's frame and the entries only report which pages were turned.

### `extractTextInRegions(buffer: Buffer, pageRegions: PageRegions[], options?: FrameOptions): PageRegionTexts[]`

Extract text within bounding-box regions from a PDF. Designed for hybrid OCR pipelines where a layout model detects regions in rendered page images, and this function extracts text from the PDF structure for text-based pages — skipping GPU OCR.

Region bboxes are `[x1, y1, x2, y2]` in PDF points with a top-left origin,
relative to the visible page box. By default they are read in the `"sheet"`
frame: the box as laid out in the content stream, `/Rotate` not applied, the
frame `extractTextWithPositions` reports items in flipped to a top-left
origin. The sheet frame matches a rendered page image only when both hold:
the page has `/Rotate 0`, and its text is not predominantly rotated. A page
whose text is predominantly rotated is turned in the sheet frame so that text
reads left-to-right (`extractTextWithPositionsAndRotations` reports which
pages were turned), so its sheet-frame bboxes do not match the rendered image
even with `/Rotate 0`. Pass `{ frame: "display" }` to give bboxes on the
rendered page (the visible box turned clockwise by the page's inheritable
`/Rotate`; the page-level turn of a predominantly rotated page is undone
first, while individual runs keep their own `rotation`), as a layout model
working on page images reports them, whatever the page's `/Rotate` or text
direction. `extractTablesInRegions` takes the same options, `boldFromWeight`
included (see `extractTextWithPositions`).

Each region result includes a `needsOcr` flag that signals unreliable extraction (empty text, GID-encoded fonts, garbage text, encoding issues). When the cause is a suspected garbled text layer, `ocrReason` is set to `"suspected_garbled_text"`.

```typescript
import { extractTextInRegions } from '@docmost/pdf-inspector'

const result = extractTextInRegions(pdf, [
  {
    page: 0, // 0-indexed
    regions: [
      [0, 0, 300, 400],    // [x1, y1, x2, y2] in PDF points, top-left origin of the visible page box (CropBox)
      [300, 0, 612, 400],
    ]
  }
])

// The same call with bboxes taken from a rendered page image
const onRendered = extractTextInRegions(
  pdf,
  [{ page: 0, regions: [[0, 0, 300, 400]] }],
  { frame: 'display' },
)

for (const region of result[0].regions) {
  if (region.needsOcr) {
    // Unreliable text — send this region to OCR instead
  } else {
    console.log(region.text) // Extracted text in reading order
  }
}
```

### `extractText(buffer: Buffer): string`

Extract plain text from a PDF.

```typescript
import { extractText } from '@docmost/pdf-inspector'

const text = extractText(readFileSync('document.pdf'))
```

### Async variants

`processPdf`, `classifyPdf`, `extractPagesMarkdown`, and `extractTextWithPositions` are synchronous and parse on the calling thread — in Node, that's the event loop. For a one-off call in a script that's fine, but in a server a large document can hold the loop for tens to hundreds of milliseconds, and an unusual one for much longer.

`processPdfAsync`, `classifyPdfAsync`, `extractPagesMarkdownAsync`, and `extractTextWithPositionsAsync` take the same arguments and produce the same results, but run the parse on the libuv thread pool and return a promise, keeping the event loop free. Invalid options still throw when the call is made. The input buffer is copied before the call returns, so it's safe to reuse or mutate immediately:

```typescript
import { classifyPdfAsync, extractPagesMarkdownAsync, extractTextWithPositionsAsync } from '@docmost/pdf-inspector'

const classification = await classifyPdfAsync(pdf)
if (classification.pdfType === 'TextBased') {
  const { pages } = await extractPagesMarkdownAsync(pdf)
  // ...
}

const items = await extractTextWithPositionsAsync(pdf, [1, 2], { frame: 'display' })
```

## Types

```typescript
interface PdfResult {
  pdfType: string          // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
  markdown: string | null  // Markdown output
  pageCount: number
  processingTimeMs: number
  pagesNeedingOcr: number[] // 1-indexed page numbers
  title: string | null
  confidence: number        // 0.0 - 1.0
  isComplexLayout: boolean
  pagesWithTables: number[]
  pagesWithColumns: number[]
  hasEncodingIssues: boolean
}

// Extends PdfResult with extracted images
interface PdfResultWithImages extends PdfResult {
  images: ExtractedImage[]  // Indices match pdf-image://N placeholders in markdown
}

interface PdfClassification {
  pdfType: string          // "TextBased" | "Scanned" | "Mixed" | "ImageBased"
  pageCount: number
  pagesNeedingOcr: number[] // 0-indexed page numbers
  confidence: number        // 0.0 - 1.0
}

interface ExtractedImage {
  page: number              // 1-indexed page number
  x: number                 // X position on page
  y: number                 // Y position on page
  width: number             // Pixel width
  height: number            // Pixel height
  format: string            // "Jpeg" | "Png"
  data: Buffer              // Raw image bytes (valid JPEG or PNG file)
}

interface PdfResult {       // processPdf / detectPdf (excerpt)
  pdfType: string
  markdown?: string         // omitted by detectPdf
  pageCount: number
  // The document information dictionary's entries, decoded as PDF text
  // strings (UTF-16 or UTF-8 after a byte order mark, PDFDocEncoding
  // otherwise); each omitted when missing or not a string.
  title?: string
  author?: string
  subject?: string
  keywords?: string
  creator?: string          // the application the document was authored in
  producer?: string         // the application that wrote the PDF
  creationDate?: string     // as written, e.g. "D:20240115103000+01'00'"
  modDate?: string
  // ...
}

interface PageRegions {
  page: number              // 0-indexed
  regions: number[][]       // [[x1, y1, x2, y2], ...] in PDF points, top-left origin of the visible page box
                            // (sheet frame by default; the rendered page with { frame: "display" })
}

interface FrameOptions {
  frame?: "sheet" | "display" // coordinate frame of items and region bboxes; "sheet" by default
  boldFromWeight?: boolean    // also read isBold from fontWeight >= boldWeightThreshold and merge runs by that verdict; false by default
  boldWeightThreshold?: number // the weight class boldFromWeight reads bold from, 100..900; 600 by default
}

interface PositionedText {
  items: TextItem[]            // as returned by extractTextWithPositions
  pageRotations: PageRotation[] // one entry per page whose text was predominantly rotated
}

interface PageRotation {
  page: number              // 1-indexed, matching TextItem.page
  rotation: string          // "ccw" | "cw": how the page was turned in the "sheet" frame
}

interface PageRegionTexts {
  page: number
  regions: RegionText[]
}

interface RegionText {
  text: string
  needsOcr: boolean         // true when text is unreliable
  ocrReason?: string        // "suspected_garbled_text" when known
}

interface OcrPdfResult {
  markdown: string
  pages: OcrPageResult[]              // 1-indexed pages + provenance
  pageCount: number
  pagesRecommendedForOcr: number[]
  pagesRoutedToOcr: number[]
  pagesRecommendingHosted: number[]
  ocrReasonsByPage: PageOcrReasons[]
  pagesWithTables: number[]
  pagesWithColumns: number[]
  isComplex: boolean
  processingTimeMs: number
  renderTimeMs: number
  ocrTimeMs: number
}
```

## Supported image formats

| PDF Filter    | Output Format | Status    |
|---------------|---------------|-----------|
| DCTDecode     | JPEG          | Supported |
| FlateDecode   | PNG           | Supported |
| JPXDecode     | JPEG2000      | Planned   |
| CCITTFaxDecode| TIFF          | Planned   |

**Color spaces:** DeviceRGB, DeviceGray, DeviceCMYK (converted to RGB), Indexed, ICCBased, CalRGB, CalGray.

**Note:** Vector graphics (drawn with PDF path operators) are not raster images and cannot be extracted — they would need to be rendered.

## Performance

Text extraction and image extraction are independent paths. `processPdf` and `extractText` skip image processing entirely, so there is zero overhead when you only need text.

## Platforms

| Platform | Architecture | Supported |
|----------|--------------|-----------|
| Linux    | x64          | Yes       |
| Linux    | ARM64        | Yes       |
| macOS    | ARM64        | Yes       |
| Windows  | x64          | Yes       |

## License

MIT

import fs from "node:fs/promises";
import path from "node:path";

import { FileBlob, PresentationFile } from "@oai/artifact-tool";

const [inputPath, previewDir] = process.argv.slice(2);

if (!inputPath || !previewDir) {
  console.error("usage: node inspect_pptx_fixture.mjs <input.pptx> <preview-dir>");
  process.exit(2);
}

const presentation = await PresentationFile.importPptx(await FileBlob.load(inputPath));
const inspection = await presentation.inspect({
  kind: "slide,textbox,shape,image,table,notes,thread,layout",
  maxChars: 16_000,
});
console.log(inspection.ndjson);

await fs.mkdir(previewDir, { recursive: true });
for (const [index, slide] of presentation.slides.items.entries()) {
  const stem = `slide-${String(index + 1).padStart(2, "0")}`;
  const png = await presentation.export({ slide, format: "png", scale: 1 });
  await fs.writeFile(
    path.join(previewDir, `${stem}.png`),
    new Uint8Array(await png.arrayBuffer()),
  );
  const layout = await slide.export({ format: "layout" });
  await fs.writeFile(path.join(previewDir, `${stem}.layout.json`), await layout.text());
}
const montage = await presentation.export({ format: "webp", montage: true, scale: 1 });
await fs.writeFile(
  path.join(previewDir, "montage.webp"),
  new Uint8Array(await montage.arrayBuffer()),
);

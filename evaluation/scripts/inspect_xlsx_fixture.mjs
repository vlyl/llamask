import fs from "node:fs/promises";
import path from "node:path";

import { FileBlob, SpreadsheetFile } from "@oai/artifact-tool";

const [inputPath, previewPath] = process.argv.slice(2);

if (!inputPath || !previewPath) {
  console.error("usage: node inspect_xlsx_fixture.mjs <input.xlsx> <preview.png>");
  process.exit(2);
}

async function main() {
  const input = await FileBlob.load(inputPath);
  const workbook = await SpreadsheetFile.importXlsx(input);

  const overview = await workbook.inspect({
    kind: "sheet,table,formula",
    maxChars: 8_000,
    tableMaxRows: 16,
    tableMaxCols: 8,
  });
  console.log(overview.ndjson);

  const formulaErrors = await workbook.inspect({
    kind: "match",
    searchTerm: "#REF!|#DIV/0!|#VALUE!|#NAME\\?|#N/A",
    options: { useRegex: true, maxResults: 100 },
    summary: "XLSX regression fixture formula error scan",
  });
  console.log(formulaErrors.ndjson);

  const firstSheet = workbook.worksheets.getItemAt(0);
  const preview = await workbook.render({
    sheetName: firstSheet.name,
    autoCrop: "all",
    scale: 1,
    format: "png",
  });
  await fs.mkdir(path.dirname(previewPath), { recursive: true });
  await fs.writeFile(previewPath, new Uint8Array(await preview.arrayBuffer()));
}

main().catch((error) => {
  console.error(`XLSX inspection failed: ${error?.message ?? String(error)}`);
  process.exit(1);
});

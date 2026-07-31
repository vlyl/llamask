import fs from "node:fs/promises";
import path from "node:path";

import { SpreadsheetFile, Workbook } from "@oai/artifact-tool";

const [outputPath, previewPath] = process.argv.slice(2);

if (!outputPath) {
  console.error("usage: node generate_xlsx_fixture.mjs <output.xlsx> [preview.png]");
  process.exit(2);
}

const workbook = Workbook.create();
const main = workbook.worksheets.add("客户数据");
const hidden = workbook.worksheets.add("内部明细");

main.showGridLines = false;
main.freezePanes.freezeRows(2);
main.getRange("A1:D1").merge();
main.getRange("A1").values = [["LlaMask XLSX 脱敏回归样本"]];
main.getRange("A1:D1").format = {
  fill: "#0F766E",
  font: { bold: true, color: "#FFFFFF", size: 16 },
  rowHeight: 30,
  verticalAlignment: "center",
};
main.getRange("A2:D2").values = [["字段", "值", "类别", "说明"]];
main.getRange("A2:D2").format = {
  fill: "#CCFBF1",
  font: { bold: true, color: "#134E4A" },
  borders: { preset: "outside", style: "thin", color: "#99F6E4" },
};
main.getRange("B3:B7").format.numberFormat = "@";
main.getRange("A3:D7").values = [
  ["联系电话", "13800000001", "个人信息", "普通共享字符串"],
  ["联系邮箱", "xlsx@example.com", "个人信息", "需要保持单元格样式"],
  ["身份证号", "110105198003150020", "个人信息", "合成校验号码"],
  ["合同编号", "HT-XLSX-2028-00008", "业务信息", "仅供离线回归"],
  ["银行卡号", "622202000000000163", "财务信息", "文本格式防止科学计数法"],
];
main.getRange("A3:D7").format.borders = {
  insideHorizontal: { style: "thin", color: "#E5E7EB" },
  bottom: { style: "thin", color: "#D1D5DB" },
};
main.getRange("A9:C9").values = [["公式场景", "公式或缓存值", "处理原则"]];
main.getRange("A9:C9").format = {
  fill: "#FEF3C7",
  font: { bold: true, color: "#92400E" },
};
main.getRange("A10").values = [["含敏感字面量的公式"]];
main.getRange("B10").formulas = [['="formula@example.com"']];
main.getRange("C10").values = [["命中后替换整格或人工保留"]];

hidden.getRange("A1:B3").values = [
  ["内部字段", "内部值"],
  ["备用电话", "13900000002"],
  ["备用邮箱", "hidden.xlsx@example.com"],
];
hidden.getRange("A1:B1").format = {
  fill: "#334155",
  font: { bold: true, color: "#FFFFFF" },
};

for (const sheet of [main, hidden]) {
  const used = sheet.getUsedRange();
  used.format.font = { name: "Arial", size: 10 };
  used.format.verticalAlignment = "center";
  used.format.autofitColumns();
  used.format.autofitRows();
}
main.getRange("A1").format.font = {
  name: "Arial",
  size: 16,
  bold: true,
  color: "#FFFFFF",
};
main.getRange("A1:D1").format.rowHeight = 30;

const overview = await workbook.inspect({
  kind: "sheet,table,formula",
  maxChars: 6_000,
  tableMaxRows: 12,
  tableMaxCols: 6,
});
console.log(overview.ndjson);

const formulaErrors = await workbook.inspect({
  kind: "match",
  searchTerm: "#REF!|#DIV/0!|#VALUE!|#NAME\\?|#N/A",
  options: { useRegex: true, maxResults: 100 },
  summary: "XLSX fixture formula error scan",
});
console.log(formulaErrors.ndjson);

if (previewPath) {
  const preview = await workbook.render({
    sheetName: "客户数据",
    autoCrop: "all",
    scale: 1,
    format: "png",
  });
  await fs.mkdir(path.dirname(previewPath), { recursive: true });
  await fs.writeFile(previewPath, new Uint8Array(await preview.arrayBuffer()));
}

await fs.mkdir(path.dirname(outputPath), { recursive: true });
const output = await SpreadsheetFile.exportXlsx(workbook);
await output.save(outputPath);

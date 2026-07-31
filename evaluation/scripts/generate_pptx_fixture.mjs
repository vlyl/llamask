import fs from "node:fs/promises";
import path from "node:path";

import { Presentation, PresentationFile } from "@oai/artifact-tool";

const [outputPath, previewDir, imagePath] = process.argv.slice(2);

if (!outputPath || !previewDir) {
  console.error(
    "usage: node generate_pptx_fixture.mjs <output.pptx> <preview-dir> [image.png]",
  );
  process.exit(2);
}

const presentation = Presentation.create({
  slideSize: { width: 1280, height: 720 },
});

function addText(slide, name, text, position, style = {}) {
  const shape = slide.shapes.add({
    geometry: "textbox",
    name,
    position,
    fill: "none",
    line: { style: "solid", fill: "none", width: 0 },
  });
  shape.text = text;
  shape.text.style = {
    typeface: "Helvetica Neue",
    color: "#000000",
    autoFit: "shrinkText",
    ...style,
  };
  return shape;
}

const cover = presentation.slides.add();
cover.background.fill = "#FFFFFF";
addText(
  cover,
  "cover-title",
  "PPTX 脱敏回归样本",
  { left: 56, top: 48, width: 1168, height: 76 },
  { fontSize: 54, bold: true },
);
addText(
  cover,
  "cover-kicker",
  "可见文本、备注、批注、隐藏页与媒体",
  { left: 56, top: 142, width: 570, height: 44 },
  { fontSize: 25, color: "#4B5563" },
);
const contact = addText(
  cover,
  "split-run-contact",
  "",
  { left: 56, top: 238, width: 570, height: 170 },
  { fontSize: 32 },
);
contact.text.set([
  [
    { run: "联系电话：", textStyle: { bold: true } },
    { run: "138" },
    { run: "0000", textStyle: { color: "#3D8DFF" } },
    { run: "0001" },
  ],
  [
    { run: "联系邮箱：", textStyle: { bold: true } },
    { run: "pptx" },
    { run: "@example", textStyle: { color: "#3D8DFF" } },
    {
      run: ".com",
      link: {
        uri: "https://example.com/contact?email=link.pptx@example.com",
        isExternal: true,
      },
    },
  ],
]);

if (imagePath) {
  const bytes = await fs.readFile(imagePath);
  const buffer = bytes.buffer.slice(
    bytes.byteOffset,
    bytes.byteOffset + bytes.byteLength,
  );
  cover.images.add({
    blob: buffer,
    contentType: "image/png",
    alt: "身份证图片 alt.pptx@example.com",
    fit: "contain",
    position: { left: 670, top: 160, width: 554, height: 420 },
    geometry: "rect",
  });
} else {
  addText(
    cover,
    "media-placeholder",
    "此区域用于嵌入图片回归",
    { left: 670, top: 260, width: 554, height: 90 },
    { fontSize: 28, color: "#6B7280", alignment: "center" },
  );
}
cover.speakerNotes.textFrame.setText(
  "演讲者备注联系人 notes.pptx@example.com，仅用于合成回归。",
);
cover.speakerNotes.setVisible(true);

const dataSlide = presentation.slides.add();
dataSlide.background.fill = "#FFFFFF";
addText(
  dataSlide,
  "data-title",
  "结构化数据也必须参与检测",
  { left: 56, top: 44, width: 1168, height: 70 },
  { fontSize: 48, bold: true },
);
addText(
  dataSlide,
  "data-subtitle",
  "表格单元格、分组形状和批注不应成为脱敏盲区。",
  { left: 56, top: 126, width: 1168, height: 44 },
  { fontSize: 23, color: "#4B5563" },
);
const table = dataSlide.tables.add({
  rows: 5,
  columns: 3,
  left: 56,
  top: 210,
  width: 1168,
  height: 340,
  columnWidths: [280, 470, 418],
  values: [
    ["字段", "合成值", "处理说明"],
    ["身份证号", "110105198003150020", "规则校验与整段替换"],
    ["银行卡号", "622202000000000163", "长数字不得遗漏"],
    ["项目邮箱", "table.pptx@example.com", "表格文字保持可编辑"],
    ["合同编号", "HT-PPTX-2028-00008", "业务策略可配置"],
  ],
});
table.styleOptions = { headerRow: true, bandedRows: true };
table.borders.assign({ style: "solid", fill: "#B8BCC4", width: 1 });
for (let column = 0; column < 3; column += 1) {
  table.getCell(0, column).fill = "#EDEDED";
  table.getCell(0, column).text.style = {
    fontSize: 19,
    bold: true,
    typeface: "Helvetica Neue",
  };
}
for (let row = 1; row < 5; row += 1) {
  for (let column = 0; column < 3; column += 1) {
    table.getCell(row, column).text.style = {
      fontSize: 18,
      typeface: "Helvetica Neue",
    };
  }
}
presentation.comments.setSelf({
  displayName: "Sensitive Reviewer",
  initials: "SR",
  email: "reviewer.pptx@example.com",
});
const thread = presentation.comments.addThread(
  { slide: dataSlide },
  "批注联系人 comment.pptx@example.com",
  { position: { x: 1140, y: 100, unit: "px" } },
);
thread.addReply("复核回复 reply.pptx@example.com");

const hiddenSlide = presentation.slides.add();
hiddenSlide.background.fill = "#FFFFFF";
addText(
  hiddenSlide,
  "hidden-title",
  "内部备用信息",
  { left: 56, top: 52, width: 1168, height: 72 },
  { fontSize: 48, bold: true },
);
addText(
  hiddenSlide,
  "hidden-content",
  "隐藏页电话 13900000002\n隐藏页邮箱 hidden.pptx@example.com",
  { left: 56, top: 210, width: 1168, height: 180 },
  { fontSize: 34 },
);

const inspection = await presentation.inspect({
  kind: "slide,textbox,shape,image,table,notes,thread,layout",
  maxChars: 12_000,
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
const montage = await presentation.export({
  format: "webp",
  montage: true,
  scale: 1,
});
await fs.writeFile(
  path.join(previewDir, "montage.webp"),
  new Uint8Array(await montage.arrayBuffer()),
);
await fs.writeFile(
  path.join(previewDir, "source-notes.txt"),
  "All fixture content is synthetic. No external claims or assets are used.\n",
);

await fs.mkdir(path.dirname(outputPath), { recursive: true });
const output = await PresentationFile.exportPptx(presentation);
await output.save(outputPath);

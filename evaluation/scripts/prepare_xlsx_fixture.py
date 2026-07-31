#!/usr/bin/env python3
"""Add deterministic OOXML edge cases to an artifact-tool-authored XLSX fixture."""

from __future__ import annotations

import argparse
import tempfile
import zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

S_NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
R_NS = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
REL_NS = "http://schemas.openxmlformats.org/package/2006/relationships"
CT_NS = "http://schemas.openxmlformats.org/package/2006/content-types"

ET.register_namespace("r", R_NS)


def qn(namespace: str, name: str) -> str:
    return f"{{{namespace}}}{name}"


def serialize(root: ET.Element, default_namespace: str) -> bytes:
    ET.register_namespace("", default_namespace)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)


def relationship(root: ET.Element, relation_id: str, relation_type: str, target: str) -> None:
    item = ET.SubElement(root, qn(REL_NS, "Relationship"))
    item.set("Id", relation_id)
    item.set("Type", relation_type)
    item.set("Target", target)


def patch_workbook(xml: bytes) -> bytes:
    root = ET.fromstring(xml)
    sheets = root.find(qn(S_NS, "sheets"))
    assert sheets is not None
    items = list(sheets)
    assert len(items) >= 2
    items[1].set("state", "hidden")
    return serialize(root, S_NS)


def replace_cell_with_shared(root: ET.Element, reference: str, index: int) -> None:
    cell = root.find(f".//{qn(S_NS, 'c')}[@r='{reference}']")
    assert cell is not None
    cell.set("t", "s")
    for child in list(cell):
        cell.remove(child)
    value = ET.SubElement(cell, qn(S_NS, "v"))
    value.text = str(index)


def replace_cell_with_inline(root: ET.Element, reference: str, text: str) -> None:
    cell = root.find(f".//{qn(S_NS, 'c')}[@r='{reference}']")
    assert cell is not None
    cell.set("t", "inlineStr")
    for child in list(cell):
        cell.remove(child)
    inline = ET.SubElement(cell, qn(S_NS, "is"))
    value = ET.SubElement(inline, qn(S_NS, "t"))
    value.text = text


def patch_sheet(xml: bytes) -> bytes:
    root = ET.fromstring(xml)
    replace_cell_with_shared(root, "B3", 0)
    replace_cell_with_inline(root, "B4", "xlsx@example.com")
    margins = root.find(qn(S_NS, "pageMargins"))
    insert_at = list(root).index(margins) if margins is not None else len(root)
    header_footer = ET.Element(qn(S_NS, "headerFooter"))
    odd_header = ET.SubElement(header_footer, qn(S_NS, "oddHeader"))
    odd_header.text = "&C内部邮箱 header.xlsx@example.com"
    root.insert(insert_at, header_footer)
    legacy = ET.Element(qn(S_NS, "legacyDrawing"))
    legacy.set(qn(R_NS, "id"), "rIdLlaMaskCommentsVml")
    root.append(legacy)
    return serialize(root, S_NS)


def shared_strings_xml() -> bytes:
    root = ET.Element(qn(S_NS, "sst"), {"count": "1", "uniqueCount": "1"})
    item = ET.SubElement(root, qn(S_NS, "si"))
    text = ET.SubElement(item, qn(S_NS, "t"))
    text.text = "13800000001"
    return serialize(root, S_NS)


def comments_xml() -> bytes:
    root = ET.Element(qn(S_NS, "comments"))
    authors = ET.SubElement(root, qn(S_NS, "authors"))
    author = ET.SubElement(authors, qn(S_NS, "author"))
    author.text = "Sensitive Reviewer"
    comments = ET.SubElement(root, qn(S_NS, "commentList"))
    comment = ET.SubElement(comments, qn(S_NS, "comment"), {"ref": "B4", "authorId": "0"})
    text = ET.SubElement(comment, qn(S_NS, "text"))
    run = ET.SubElement(text, qn(S_NS, "r"))
    value = ET.SubElement(run, qn(S_NS, "t"))
    value.text = "批注联系人 comment.xlsx@example.com"
    return serialize(root, S_NS)


def vml_xml() -> bytes:
    return b'''<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel">
  <v:shape id="_x0000_s1025" type="#_x0000_t202" style="position:absolute;visibility:hidden">
    <x:ClientData ObjectType="Note"><x:Row>3</x:Row><x:Column>1</x:Column></x:ClientData>
  </v:shape>
</xml>'''


def patch_sheet_relationships(xml: bytes) -> bytes:
    root = ET.fromstring(xml)
    relationship(
        root,
        "rIdLlaMaskComments",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments",
        "../comments1.xml",
    )
    relationship(
        root,
        "rIdLlaMaskCommentsVml",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing",
        "../drawings/vmlDrawing1.vml",
    )
    return serialize(root, REL_NS)


def empty_relationships_xml() -> bytes:
    root = ET.Element(qn(REL_NS, "Relationships"))
    return serialize(root, REL_NS)


def patch_content_types(xml: bytes) -> bytes:
    root = ET.fromstring(xml)
    override = ET.SubElement(root, qn(CT_NS, "Override"))
    override.set("PartName", "/xl/comments1.xml")
    override.set(
        "ContentType",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
    )
    if not any(item.get("Extension") == "vml" for item in root.findall(qn(CT_NS, "Default"))):
        default = ET.SubElement(root, qn(CT_NS, "Default"))
        default.set("Extension", "vml")
        default.set("ContentType", "application/vnd.openxmlformats-officedocument.vmlDrawing")
    return serialize(root, CT_NS)


def build(source: Path, output: Path) -> None:
    with zipfile.ZipFile(source, "r") as archive:
        entries = {name: archive.read(name) for name in archive.namelist()}
    entries["xl/workbook.xml"] = patch_workbook(entries["xl/workbook.xml"])
    entries["xl/worksheets/sheet1.xml"] = patch_sheet(entries["xl/worksheets/sheet1.xml"])
    entries["xl/sharedStrings.xml"] = shared_strings_xml()
    entries["xl/comments1.xml"] = comments_xml()
    entries["xl/drawings/vmlDrawing1.vml"] = vml_xml()
    sheet_relationships = entries.get(
        "xl/worksheets/_rels/sheet1.xml.rels", empty_relationships_xml()
    )
    entries["xl/worksheets/_rels/sheet1.xml.rels"] = patch_sheet_relationships(
        sheet_relationships
    )
    entries["[Content_Types].xml"] = patch_content_types(entries["[Content_Types].xml"])

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False, dir=output.parent) as handle:
        temporary = Path(handle.name)
    try:
        with zipfile.ZipFile(temporary, "w", compression=zipfile.ZIP_DEFLATED) as target:
            for name in sorted(entries):
                info = zipfile.ZipInfo(name, date_time=(2000, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED
                info.create_system = 3
                info.external_attr = 0o100644 << 16
                target.writestr(info, entries[name])
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    build(args.source, args.output)
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

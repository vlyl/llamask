#!/usr/bin/env python3
"""Add deterministic hidden/master/layout edge cases to a PPTX fixture."""

from __future__ import annotations

import argparse
import tempfile
import zipfile
from pathlib import Path
from xml.etree import ElementTree as ET

P_NS = "http://schemas.openxmlformats.org/presentationml/2006/main"
A_NS = "http://schemas.openxmlformats.org/drawingml/2006/main"

ET.register_namespace("p", P_NS)
ET.register_namespace("a", A_NS)


def qn(namespace: str, name: str) -> str:
    return f"{{{namespace}}}{name}"


def serialize(root: ET.Element) -> bytes:
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)


def hidden_slide(xml: bytes) -> bytes:
    root = ET.fromstring(xml)
    root.set("show", "0")
    return serialize(root)


def off_canvas_text_shape(shape_id: int, name: str, text: str) -> ET.Element:
    shape = ET.Element(qn(P_NS, "sp"))
    non_visual = ET.SubElement(shape, qn(P_NS, "nvSpPr"))
    ET.SubElement(
        non_visual,
        qn(P_NS, "cNvPr"),
        {"id": str(shape_id), "name": name},
    )
    ET.SubElement(non_visual, qn(P_NS, "cNvSpPr"), {"txBox": "1"})
    ET.SubElement(non_visual, qn(P_NS, "nvPr"))

    properties = ET.SubElement(shape, qn(P_NS, "spPr"))
    transform = ET.SubElement(properties, qn(A_NS, "xfrm"))
    ET.SubElement(transform, qn(A_NS, "off"), {"x": "-9144000", "y": "-9144000"})
    ET.SubElement(transform, qn(A_NS, "ext"), {"cx": "914400", "cy": "457200"})
    geometry = ET.SubElement(properties, qn(A_NS, "prstGeom"), {"prst": "rect"})
    ET.SubElement(geometry, qn(A_NS, "avLst"))
    ET.SubElement(properties, qn(A_NS, "noFill"))
    line = ET.SubElement(properties, qn(A_NS, "ln"))
    ET.SubElement(line, qn(A_NS, "noFill"))

    text_body = ET.SubElement(shape, qn(P_NS, "txBody"))
    ET.SubElement(text_body, qn(A_NS, "bodyPr"))
    ET.SubElement(text_body, qn(A_NS, "lstStyle"))
    paragraph = ET.SubElement(text_body, qn(A_NS, "p"))
    run = ET.SubElement(paragraph, qn(A_NS, "r"))
    ET.SubElement(run, qn(A_NS, "rPr"), {"lang": "zh-CN"})
    value = ET.SubElement(run, qn(A_NS, "t"))
    value.text = text
    ET.SubElement(paragraph, qn(A_NS, "endParaRPr"), {"lang": "zh-CN"})
    return shape


def inject_structural_text(xml: bytes, shape_id: int, name: str, text: str) -> bytes:
    root = ET.fromstring(xml)
    tree = root.find(f".//{qn(P_NS, 'spTree')}")
    assert tree is not None
    tree.append(off_canvas_text_shape(shape_id, name, text))
    return serialize(root)


def build(source: Path, output: Path) -> None:
    with zipfile.ZipFile(source, "r") as archive:
        entries = {name: archive.read(name) for name in archive.namelist()}

    entries["ppt/slides/slide3.xml"] = hidden_slide(entries["ppt/slides/slide3.xml"])
    entries["ppt/slideMasters/slideMaster1.xml"] = inject_structural_text(
        entries["ppt/slideMasters/slideMaster1.xml"],
        9001,
        "Master Regression Text",
        "母版邮箱 master.pptx@example.com",
    )
    entries["ppt/slideLayouts/slideLayout1.xml"] = inject_structural_text(
        entries["ppt/slideLayouts/slideLayout1.xml"],
        9002,
        "Layout Regression Text",
        "版式电话 13700000003",
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(suffix=".pptx", delete=False, dir=output.parent) as handle:
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

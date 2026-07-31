#!/usr/bin/env python3
"""Generate a synthetic PDF covering visible, scanned, hidden, and interactive data."""

from __future__ import annotations

import argparse
import tempfile
from pathlib import Path
from unittest.mock import patch

from PIL import Image, ImageDraw, ImageFont
from pypdf import PdfReader, PdfWriter
from reportlab.lib.colors import HexColor, black
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas
from reportlab.lib.utils import ImageReader


def scanned_image() -> Image.Image:
    image = Image.new("RGB", (1500, 900), "#f7f7f5")
    draw = ImageDraw.Draw(image)
    font = ImageFont.load_default(size=34)
    title = ImageFont.load_default(size=54)
    draw.text((90, 90), "SCANNED DOCUMENT", fill="#111111", font=title)
    draw.text((90, 220), "ID: 110105198003150020", fill="#111111", font=font)
    draw.text((90, 310), "Email: scan.pdf@example.com", fill="#111111", font=font)
    draw.rectangle((80, 195, 1120, 380), outline="#c2c2c2", width=3)
    draw.text((90, 720), "Synthetic regression data only", fill="#666666", font=font)
    return image


def base_pdf(path: Path) -> None:
    document = canvas.Canvas(str(path), pagesize=A4, pageCompression=1, invariant=1)
    width, height = A4
    document.setTitle("Sensitive PDF Regression Fixture")
    document.setAuthor("Sensitive Author")
    document.setSubject("metadata.pdf@example.com")

    document.setFillColor(black)
    document.setFont("Helvetica-Bold", 28)
    document.drawString(54, height - 70, "PDF Redaction Regression Fixture")
    document.setFont("Helvetica", 15)
    document.setFillColor(HexColor("#4b5563"))
    document.drawString(
        54,
        height - 104,
        "Native text, forms, annotations, scanned pages, and hidden objects",
    )
    document.setFillColor(black)
    document.setFont("Helvetica", 19)
    document.drawString(54, height - 180, "Phone: 13800000001")
    document.drawString(54, height - 220, "Email: native.pdf@example.com")
    document.setFont("Helvetica", 13)
    document.drawString(54, height - 285, "Form value:")
    document.acroForm.textfield(
        name="contact_email",
        value="form.pdf@example.com",
        x=130,
        y=height - 305,
        width=260,
        height=28,
        borderWidth=1,
        borderColor=HexColor("#9ca3af"),
        fillColor=HexColor("#ffffff"),
        textColor=black,
        fontName="Helvetica",
        fontSize=12,
    )
    document.textAnnotation(
        "Comment contact comment.pdf@example.com",
        Rect=(420, height - 305, 442, height - 283),
        name="Synthetic Review",
    )
    document.setFillColor(HexColor("#2563eb"))
    document.drawString(54, height - 355, "External link (must be removed after export)")
    document.linkURL(
        "https://example.com/contact?email=link.pdf@example.com",
        (54, height - 360, 280, height - 338),
        relative=0,
    )
    document.setFillColor(HexColor("#6b7280"))
    document.drawString(54, 48, "Page 1 - native content")
    document.showPage()

    image = scanned_image()
    document.setFont("Helvetica-Bold", 25)
    document.setFillColor(black)
    document.drawString(54, height - 70, "Scanned page")
    document.drawImage(
        ImageReader(image),
        54,
        150,
        width=width - 108,
        height=height - 260,
        preserveAspectRatio=True,
        anchor="c",
        mask="auto",
    )
    document.setFont("Helvetica", 13)
    document.setFillColor(HexColor("#6b7280"))
    document.drawString(54, 48, "Page 2 - image content")
    document.showPage()

    document.setFont("Helvetica-Bold", 25)
    document.setFillColor(black)
    document.drawString(54, height - 70, "Invisible content and container risks")
    document.setFont("Helvetica", 16)
    document.drawString(54, height - 130, "The visible page area contains no sensitive data.")
    hidden = document.beginText(54, height - 190)
    hidden.setFont("Helvetica", 12)
    hidden.setTextRenderMode(3)
    hidden.textLine("hidden.pdf@example.com 13900000002")
    document.drawText(hidden)
    document.setFont("Helvetica", 13)
    document.setFillColor(HexColor("#6b7280"))
    document.drawString(54, 48, "Page 3 - hidden text, attachments, and scripts")
    document.showPage()
    document.save()


def build(output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as directory:
        base = Path(directory) / "base.pdf"
        base_pdf(base)
        reader = PdfReader(base)
        writer = PdfWriter()
        writer.clone_document_from_reader(reader)
        writer.add_metadata(
            {
                "/Title": "Sensitive PDF Regression Fixture",
                "/Author": "Sensitive Author",
                "/Subject": "metadata.pdf@example.com",
                "/Keywords": "internal, synthetic",
            }
        )
        writer.add_attachment(
            "internal-note.txt",
            b"Synthetic attachment contact attachment.pdf@example.com",
        )
        # pypdf normally assigns a random UUID to the JavaScript name tree.
        with patch("pypdf._writer.uuid.uuid4", return_value="llamask-pdf-fixture-js"):
            writer.add_js("app.alert('Synthetic js.pdf@example.com');")
        with output.open("wb") as handle:
            writer.write(handle)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    build(args.output)
    print(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

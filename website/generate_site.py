#!/usr/bin/env python3
"""Regenerate the static site from the project README."""

from pathlib import Path
from shutil import copytree, rmtree
import re
from string import Template

import markdown

WEBSITE_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = WEBSITE_DIR.parent
CONTENT_DIR = WEBSITE_DIR / 'content'
README_MD = PROJECT_ROOT / 'README.md'
TEMPLATE_PATH = WEBSITE_DIR / 'index.html.template'
OUTPUT_HTML = CONTENT_DIR / 'index.html'
DOCS_SOURCE = PROJECT_ROOT / 'docs'
DOCS_TARGET = CONTENT_DIR / 'docs'

MD_EXTENSIONS = [
    'fenced_code',
    'tables',
    'toc',
    'sane_lists',
]

STRIKE_PATTERN = re.compile(r'~~(.*?)~~', re.DOTALL)
LOGO_PATTERN = re.compile(
    r'<p align="center">\s*<img src="docs/images/logo/ascii-art-boon\.png" alt="boon logo" ?/?>\s*</p>',
    re.IGNORECASE,
)
TAGLINE_PATTERN = re.compile(
    r'<p align="center">\s*Timeless\s*&(?:amp;)?\s*Playful Language\s*</p>',
    re.IGNORECASE,
)


def sync_docs() -> None:
    if DOCS_TARGET.exists():
        rmtree(DOCS_TARGET)
    copytree(DOCS_SOURCE, DOCS_TARGET)


def render_readme() -> str:
    text = README_MD.read_text()
    text = STRIKE_PATTERN.sub(r'<del>\1</del>', text)
    html = markdown.markdown(text, extensions=MD_EXTENSIONS)
    html = LOGO_PATTERN.sub(
        '<p class="readme__logo"><img src="docs/images/logo/ascii-art-boon.png" alt="boon logo" /></p>',
        html,
        count=1,
    )
    html = TAGLINE_PATTERN.sub(
        '<p class="readme__tagline">Timeless &amp; Playful Language</p>',
        html,
        count=1,
    )
    return html


def build_page() -> str:
    template = Template(TEMPLATE_PATH.read_text())
    return template.substitute(readme=render_readme())


def main() -> None:
    sync_docs()
    OUTPUT_HTML.write_text(build_page())
    print(f'Regenerated {OUTPUT_HTML.relative_to(PROJECT_ROOT)} from {README_MD.relative_to(PROJECT_ROOT)}')


if __name__ == '__main__':
    main()

"""HTML → markdown extraction for ИТС pages. Pure, selector-driven, testable."""
import re
from bs4 import BeautifulSoup
from markdownify import markdownify as md


def extract_content(html: str, content_selector: str, strip_selectors: list[str]) -> dict:
    soup = BeautifulSoup(html, "html.parser")
    title = (soup.title.string.strip() if soup.title and soup.title.string else "")

    root = soup.select_one(content_selector) or soup.body or soup
    for sel in strip_selectors:
        for el in root.select(sel):
            el.decompose()

    images_omitted = 0
    for img in root.find_all("img"):
        alt = img.get("alt", "").strip()
        images_omitted += 1
        img.replace_with(f"[изображение: {alt}]")

    markdown = md(str(root), heading_style="ATX", strip=["script", "style"])
    markdown = re.sub(r"\n{3,}", "\n\n", markdown).strip()
    return {"title": title, "markdown": markdown, "images_omitted": images_omitted}


def parse_search_results(html: str, cfg: dict) -> list[dict]:
    soup = BeautifulSoup(html, "html.parser")
    rows: list[dict] = []
    for node in soup.select(cfg["result"]):
        t = node.select_one(cfg["title"])
        s = node.select_one(cfg["snippet"])
        link = node.select_one(cfg["link"])
        ref = (link.get("href") if link else "") or ""
        m = re.search(r"/db/([^/#?]+)", ref)
        rows.append({
            "title": (t.get_text(strip=True) if t else ""),
            "snippet": (s.get_text(strip=True) if s else ""),
            "ref": ref,
            "db": (m.group(1) if m else ""),
        })
    return rows

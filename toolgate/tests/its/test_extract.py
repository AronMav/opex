from its.extract import extract_content, parse_search_results

CONTENT_HTML = """
<html><head><title>Регламентные задания</title></head><body>
  <nav class="toc">меню</nav>
  <header>шапка</header>
  <div id="content">
    <h1>Регламентные задания</h1>
    <p>Первый абзац.</p>
    <img src="/x.png" alt="схема">
    <table><tr><th>A</th></tr><tr><td>1</td></tr></table>
  </div>
  <footer>подвал</footer>
</body></html>
"""

def test_extract_strips_nav_and_keeps_content():
    r = extract_content(CONTENT_HTML, content_selector="#content",
                        strip_selectors=["nav", "header", "footer"])
    assert "Регламентные задания" in r["markdown"]
    assert "Первый абзац" in r["markdown"]
    assert "меню" not in r["markdown"]
    assert "подвал" not in r["markdown"]

def test_extract_image_placeholder():
    r = extract_content(CONTENT_HTML, content_selector="#content",
                        strip_selectors=["nav", "header", "footer"])
    assert "[изображение: схема]" in r["markdown"]
    assert r["images_omitted"] == 1

def test_extract_title():
    r = extract_content(CONTENT_HTML, content_selector="#content", strip_selectors=[])
    assert r["title"] == "Регламентные задания"

SEARCH_HTML = """
<div class="search-results">
  <div class="result"><a class="r-link" href="/db/v854doc#bookmark:adm:TI1">Тема 1</a>
    <span class="r-snip">описание 1</span></div>
  <div class="result"><a class="r-link" href="/db/v854doc#bookmark:adm:TI2">Тема 2</a>
    <span class="r-snip">описание 2</span></div>
</div>
"""
SEARCH_CFG = {"result": "div.result", "title": "a.r-link", "snippet": "span.r-snip", "link": "a.r-link"}

def test_parse_search_results():
    rows = parse_search_results(SEARCH_HTML, SEARCH_CFG)
    assert len(rows) == 2
    assert rows[0]["title"] == "Тема 1"
    assert rows[0]["snippet"] == "описание 1"
    assert rows[0]["ref"] == "/db/v854doc#bookmark:adm:TI1"
    assert rows[0]["db"] == "v854doc"

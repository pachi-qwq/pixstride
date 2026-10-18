"""Check the public tree with Python's standard library; never modify files.

Supported Markdown: ATX headings, backtick/tilde fences, indented code, inline code,
inline/reference/shortcut links, autolinks, HTML comments and explicit anchor IDs.
This is a focused GFM subset. Prose advice is non-blocking; privacy scans raw text.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
import ipaddress
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import unquote, urldefrag, urlsplit
from urllib.request import Request, urlopen


@dataclass(frozen=True)
class Finding:
    severity: str
    path: str
    line: int
    rule: str
    message: str


@dataclass(frozen=True)
class Link:
    target: str
    line: int


@dataclass
class Markdown:
    links: list[Link] = field(default_factory=list)
    anchors: set[str] = field(default_factory=set)
    findings: list[Finding] = field(default_factory=list)


CJK = r"\u3400-\u4dbf\u4e00-\u9fff"
METADATA = re.compile(r"<!-- doc-id: ([a-z-]+); revision: (\d+); status: ([a-z-]+) -->")
PRIVATE_PATTERNS = {
    "private-path": r"(?i)(?:\b[a-z]:[\\/]|/(?:Users|home|tmp|var/tmp)/|\bOneDrive[\\/])",
    "private-key": r"-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----",
    "github-token": r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{40,})\b",
    "cloud-key": r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
    "bearer-token": r"(?i)\bBearer\s+[A-Za-z0-9_.+/=-]{24,}",
    "url-credentials": r"https?://[^\s/@]+:[^\s/@]+@",
}
PRIVATE_DIRS = {"internal", "private", ".local", "test-data", "samples", "logs", "cache", "caches", "artifacts", "target", "generated", "stress-data"}
IGNORED_FALLBACK = {".git", ".idea", ".vscode", "__pycache__"}
IMAGES = {".png", ".jpg", ".jpeg", ".gif", ".tif", ".tiff", ".webp", ".avif", ".heic", ".heif", ".jxl", ".raw", ".dng", ".psd", ".psb", ".bmp", ".ico", ".tga", ".pcx", ".apng"}
MEDIA = IMAGES | {".etl", ".dmp", ".zip", ".7z"}
MAX_BYTES = 1024 * 1024


def mask_code(text: str) -> str:
    """Replace code spans with spaces while preserving offsets and newlines."""
    return re.sub(r"(`+)([^`]*?|(?:[^`]|`(?!`))*?)\1(?!`)",
                  lambda m: re.sub(r"[^\n]", " ", m.group()), text)


def slug(text: str) -> str:
    text = re.sub(r"\[([^]]+)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]*>", "", text).replace("`", "")
    return re.sub(r"[^\w\- ]", "", text.lower()).replace(" ", "-")


def balanced(text: str, start: int, opening: str, closing: str) -> int:
    depth, i, quoted, angle = 1, start + 1, False, False
    while i < len(text):
        char = text[i]
        if char == "\\":
            i += 2
            continue
        if opening == "(" and char == '"' and (quoted or text[i-1].isspace()):
            quoted = not quoted
        if not quoted:
            if opening == "(" and char == "<":
                angle = True
            elif angle and char == ">":
                angle = False
            elif not angle and char == opening:
                depth += 1
            elif not angle and char == closing:
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return -1


def destination(text: str) -> str:
    text = text.strip()
    if text.startswith("<") and ">" in text:
        return text[1:text.index(">")]
    target = re.split(r"\s+", text, maxsplit=1)[0] if text else ""
    return re.sub(r"\\([\\`*_{}\[\]()#+\-.!<> ])", r"\1", target)


def extract_links(text: str) -> tuple[list[Link], list[tuple[int, str]]]:
    refs, links, errors = {}, [], []
    lines = text.splitlines(keepends=True)
    for index, line in enumerate(lines):
        match = re.match(r"^ {0,3}\[([^]]+)\]:\s*(.+)$", line)
        if match:
            key = " ".join(match[1].split()).casefold()
            refs[key] = destination(match[2])
            links.append(Link(refs[key], index + 1))
            lines[index] = "\n" if line.endswith("\n") else ""
    text = "".join(lines)
    i = 0
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] != "[":
            i += 1
            continue
        end = balanced(text, i, "[", "]")
        if end < 0:
            i += 1
            continue
        label = " ".join(text[i+1:end].split()).casefold()
        after = end + 1
        while after < len(text) and text[after].isspace():
            after += 1
        line = text.count("\n", 0, i) + 1
        if after < len(text) and text[after] == "(":
            close = balanced(text, after, "(", ")")
            if close < 0:
                errors.append((line, "Unclosed inline link destination"))
                i = end + 1
                continue
            links.append(Link(destination(text[after+1:close]), line))
            i = close + 1
        elif after < len(text) and text[after] == "[":
            close = balanced(text, after, "[", "]")
            if close < 0:
                errors.append((line, "Unclosed reference link"))
                i = end + 1
                continue
            key = " ".join(text[after+1:close].split()).casefold() or label
            if key not in refs:
                errors.append((line, "Undefined reference link"))
            else:
                links.append(Link(refs[key], line))
            i = close + 1
        else:
            if label in refs:
                links.append(Link(refs[label], line))
            i = end + 1
    for match in re.finditer(r"<((?:https?://|mailto:)[^<>\s]+)>", text):
        links.append(Link(match[1], text.count("\n", 0, match.start()) + 1))
    return links, errors


def prose_advice(text: str) -> list[tuple[str, str]]:
    text = re.sub(r"\[[^]]*\]\([^)]*\)", lambda m: m[0].split("]", 1)[0][1:], text)
    text = re.sub(r'https?://\S+|“[^”]*”|"[^"]*"', " ", text)
    advice = []
    if re.search(rf"[{CJK}][A-Za-z0-9]|[A-Za-z0-9][{CJK}]", text):
        advice.append(("zh-spacing", "Review spacing between Chinese and Latin words/numbers"))
    if re.search(rf"[{CJK}][,:;!?]|[,;!?][{CJK}]", text):
        advice.append(("zh-punctuation", "Review ASCII punctuation in a Chinese sentence"))
    if re.search(r"\d(?:MiB|GiB|GB|ms)\b", text):
        advice.append(("unit-spacing", "Separate values from byte/time units"))
    if re.search(r"赋能|抓手|深度融合|具有重要意义|全面提升", text):
        advice.append(("prose-specificity", "Review vague emphasis in context; do not replace mechanically"))
    for misspelling in ("Direct3d", "D3d11", "Github", "Window-rs"):
        if re.search(r"\b" + misspelling + r"\b", text):
            advice.append(("term-spelling", "Review the official technical spelling against the glossary"))
            break
    return advice


def parse_markdown(content: str, path: str) -> Markdown:
    result = Markdown()
    add = lambda line, rule, msg, level="error": result.findings.append(Finding(level, path, line, rule, msg))
    clean = re.sub(r"<!--[\s\S]*?-->", lambda m: re.sub(r"[^\n]", " ", m[0]), content)
    raw_lines, clean_lines = content.splitlines(), clean.splitlines()
    fence, fence_line, h1, last_level, frontmatter = None, 0, 0, 0, False
    counts, link_lines = {}, []
    for index, line in enumerate(clean_lines):
        number, stripped = index + 1, line.strip()
        link_lines.append("")
        if index == 0 and stripped == "---":
            frontmatter = True
            continue
        if frontmatter:
            if stripped == "---":
                frontmatter = False
            continue
        marker = re.match(r"^ {0,3}(`{3,}|~{3,})(.*)$", line)
        if fence:
            if marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence) and not marker[2].strip():
                fence = None
                if index + 1 < len(clean_lines) and clean_lines[index+1].strip():
                    add(number, "block-spacing", "Leave a blank line after a fenced block")
            continue
        if marker:
            fence, fence_line = marker[1], number
            if not marker[2].strip():
                add(number, "fence-language", "Specify a fenced-block language")
            if index and clean_lines[index-1].strip():
                add(number, "block-spacing", "Leave a blank line before a fenced block")
            continue
        if line.startswith(("    ", "\t")):
            continue
        heading = re.match(r"^ {0,3}(#{1,6})\s+(.+?)(?:\s+#+)?$", line)
        if heading:
            level = len(heading[1])
            h1 += level == 1
            if level > last_level + 1:
                add(number, "heading-level", "Do not skip heading levels")
            last_level = level
            anchor = slug(heading[2])
            ordinal = counts.get(anchor, 0)
            counts[anchor] = ordinal + 1
            result.anchors.add(anchor + (f"-{ordinal}" if ordinal else ""))
            if (index and clean_lines[index-1].strip()) or (index+1 < len(clean_lines) and clean_lines[index+1].strip()):
                add(number, "heading-spacing", "Leave blank lines around headings")
        elif re.match(r"^ {0,3}#{1,6}[^#\s]", line):
            add(number, "heading-space", "Put a space after the heading marker")
        list_item = re.match(r"^ {0,3}(?:[-+*]|\d+[.)])\s+", line)
        previous = clean_lines[index-1] if index else ""
        if list_item and previous.strip() and not re.match(r"^\s*(?:[-+*]|\d+[.)])\s+", previous) and not previous.startswith("  "):
            add(number, "list-spacing", "Leave a blank line before a list")
        if stripped.startswith("|") and index and previous.strip() and not previous.strip().startswith("|"):
            add(number, "table-spacing", "Leave a blank line before a table")
        link_lines[-1] = mask_code(line)
        if not stripped.startswith(">"):
            if re.search(rf"[{CJK}]`+|`+[{CJK}]", line):
                add(number, "zh-code-spacing", "Review spacing around inline code", "warning")
            for rule, msg in prose_advice(mask_code(line)):
                add(number, rule, msg, "warning")
    if fence:
        add(fence_line, "fence-unclosed", "Unclosed fenced block")
    if frontmatter:
        add(1, "frontmatter", "Unclosed front matter")
    if h1 != 1:
        add(1, "heading-h1", "Use exactly one H1")
    result.anchors.update(re.findall(r'<(?:a|span)\s+(?:id|name)=["\']([^"\']+)["\']', clean))
    result.links, errors = extract_links("\n".join(link_lines))
    for line, msg in errors:
        add(line, "link-syntax", msg)
    for number, line in enumerate(raw_lines, 1):
        if line.rstrip() != line:
            add(number, "trailing-whitespace", "Remove trailing whitespace; use explicit line breaks")
    return result


def scan_raw(content: str, path: str) -> list[Finding]:
    findings = []
    for number, line in enumerate(content.splitlines(), 1):
        for rule, pattern in PRIVATE_PATTERNS.items():
            if re.search(pattern, line):
                findings.append(Finding("error", path, number, rule, "Potential private data; inspect locally (value redacted)"))
    return findings


def public_files(root: Path) -> list[Path]:
    git = subprocess.run(["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard", "-z"], capture_output=True)
    if git.returncode == 0:
        names = set(git.stdout.decode("utf-8").split("\0")) - {""}
        return [root / name for name in sorted(names)]
    return sorted(p for p in root.rglob("*") if p.is_file() and not IGNORED_FALLBACK.intersection(p.relative_to(root).parts))


def exact_case(path: Path, root: Path) -> bool:
    parent = root
    for part in path.relative_to(root).parts:
        if part not in {child.name for child in parent.iterdir()}:
            return False
        parent /= part
    return True


def check_tree(root: Path) -> tuple[list[Finding], list[tuple[str, str, int]], int]:
    root = root.resolve()
    findings, pages, contents, external = [], {}, {}, []
    files = public_files(root)
    add = lambda path, line, rule, msg: findings.append(Finding("error", path, line, rule, msg))
    for file in files:
        path = file.relative_to(root).as_posix()
        parts = file.relative_to(root).parts
        image = file.suffix.lower() in IMAGES
        image_location = parts[0] == "assets" or parts[:2] == ("tests", "fixtures")
        if not file.resolve().is_relative_to(root) or file.is_symlink():
            add(path, 1, "public-symlink", "Public files must not escape the repository or use symlinks")
            continue
        if not file.exists():
            add(path, 1, "missing-file", "Tracked file is missing; inspect the intended deletion")
            continue
        if PRIVATE_DIRS.intersection(parts) or parts[:2] in {("tests", "local"), ("tests", "output")} or (file.name.startswith(".env") and file.name != ".env.example") or (file.suffix.lower() in MEDIA and not (image and image_location)):
            add(path, 1, "private-artifact", "Private/generated media or configuration must not be in the public tree")
        if file.stat().st_size > MAX_BYTES:
            add(path, 1, "file-size", "Public files over 1 MiB need an explicit fixture/policy review")
            continue
        if image:
            # Never decode resources in documentation CI. Review provenance and embedded metadata manually.
            if image_location:
                contents[path] = ""
            continue
        try:
            data = file.read_bytes()
            content = data.decode("utf-8")
        except UnicodeDecodeError:
            add(path, 1, "text-encoding", "Unexpected non-UTF-8/binary file in the current documentation tree")
            continue
        if b"\r" in data or data.startswith(b"\xef\xbb\xbf") or (data and not data.endswith(b"\n")):
            add(path, 1, "text-format", "Use UTF-8 without BOM, LF, and a final newline")
        findings.extend(scan_raw(content, path))
        contents[path] = content
        if file.suffix.lower() == ".md":
            pages[path] = parse_markdown(content, path)
            findings.extend(pages[path].findings)
    for path, page in pages.items():
        for link in page.links:
            if not link.target:
                add(path, link.line, "link-empty", "Link target must not be empty")
                continue
            parts = urlsplit(link.target)
            if parts.scheme in {"https", "http"}:
                host = parts.hostname or ""
                try:
                    address = ipaddress.ip_address(host)
                except ValueError:
                    address = None
                if not host or host == "localhost" or (address and not address.is_global):
                    add(path, link.line, "private-host", "Do not publish local/private network links")
                else:
                    external.append((urldefrag(link.target)[0], path, link.line))
                continue
            if parts.scheme == "mailto":
                continue
            if parts.scheme or parts.netloc or parts.path.startswith(("/", "\\")):
                add(path, link.line, "link-local", "Use repository-relative local links or HTTPS")
                continue
            requested = (root / path).parent.joinpath(unquote(parts.path)) if parts.path else root / path
            target = Path(os.path.abspath(requested))
            if not target.is_relative_to(root) or not target.exists() or not exact_case(target, root) or not target.resolve().is_relative_to(root):
                add(path, link.line, "link-target", "Missing, case-mismatched or outside-repository local link")
                continue
            target_name = target.relative_to(root).as_posix()
            if (target.is_file() and target_name not in contents) or PRIVATE_DIRS.intersection(target.relative_to(root).parts) or IGNORED_FALLBACK.intersection(target.relative_to(root).parts):
                add(path, link.line, "link-private", "Public links must not point to ignored/private material")
                continue
            if parts.fragment and target_name in pages and unquote(parts.fragment) not in pages[target_name].anchors:
                add(path, link.line, "link-fragment", "Local heading/anchor does not exist")
    catalog_path = root / "docs/document-pairs.json"
    if not catalog_path.is_file():
        add("docs/document-pairs.json", 1, "pair-catalog", "Document pair catalog is missing")
        return findings, external, len(files)
    try:
        catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
        if catalog["schema_version"] != 1:
            raise ValueError("Unsupported schema")
        registered, ids = set(), set()
        for pair in catalog["pairs"]:
            if pair["id"] in ids:
                add("docs/document-pairs.json", 1, "pair-id", "Duplicate document ID")
            ids.add(pair["id"])
            pair_paths = [pair["en"], pair["zh-CN"]]
            for path in pair_paths:
                if path in registered:
                    add(path, 1, "pair-duplicate", "Document appears in multiple pairs")
                registered.add(path)
                text = contents.get(path, "")
                meta = METADATA.search(text)
                if not meta or meta[1] != pair["id"] or int(meta[2]) != pair["revision"]:
                    add(path, 1, "pair-revision", "Document ID/revision must match its catalog entry")
                first = text.splitlines()[0] if text else ""
                first_links, _ = extract_links(first)
                destinations = {(root / path).parent.joinpath(unquote(urlsplit(link.target).path)).resolve() for link in first_links}
                if "[English]" not in first or "[简体中文]" not in first or destinations != {root / item for item in pair_paths}:
                    add(path, 1, "language-entry", "Top language links must point to both members of this pair")
                for literal in pair.get("shared_literals", []):
                    if literal not in text:
                        add(path, 1, "shared-literal", "A reviewed shared technical literal is missing")
            metas = [METADATA.search(contents.get(p, "")) for p in pair_paths]
            if all(metas) and metas[0][3] != metas[1][3]:
                add(pair_paths[1], 1, "pair-status", "Language versions must share the same status")
        for path in catalog.get("bilingual_templates", []):
            registered.add(path)
            text = contents.get(path, "")
            if path not in pages or not re.search(rf"[{CJK}]", text) or not re.search(r"[A-Za-z]{3}", text):
                add(path, 1, "template-language", "Template must provide English and Simplified Chinese prompts")
        for path in set(pages) - registered:
            add(path, 1, "unpaired-document", "Register an English/Chinese pair or a bilingual template")
    except (ValueError, KeyError, TypeError) as exc:
        add("docs/document-pairs.json", 1, "pair-catalog", f"Invalid pair catalog ({type(exc).__name__})")
    return findings, external, len(files)


def check_url(url: str) -> tuple[str, str]:
    for method in ("HEAD", "GET"):
        try:
            headers = {"User-Agent": "Pixstride-documentation-check/1"}
            if method == "GET":
                headers["Range"] = "bytes=0-0"
            with urlopen(Request(url, headers=headers, method=method), timeout=12) as response:
                return "ok", str(response.status)
        except HTTPError as exc:
            if method == "HEAD":
                continue
            if exc.code in {404, 410}:
                return "error", f"HTTP {exc.code}"
            return "warning", f"HTTP {exc.code}; manual review required"
        except (URLError, TimeoutError, OSError, ValueError):
            if method == "HEAD":
                continue
            return "warning", "Transport failure; manual review required"
    return "warning", "Reachability not confirmed"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--external-links", action="store_true")
    parser.add_argument("--report", type=Path, help="Optional JSON report outside the repository")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.report and args.report.resolve().is_relative_to(root):
        parser.error("Write generated reports outside the repository")
    findings, links, count = check_tree(root)
    network = {}
    if args.external_links and not any(item.severity == "error" for item in findings):
        urls = sorted({link[0] for link in links})
        with ThreadPoolExecutor(max_workers=4) as pool:
            network = dict(zip(urls, pool.map(check_url, urls)))
        for url, path, line in links:
            level, message = network[url]
            if level != "ok":
                findings.append(Finding(level, path, line, "external-link", f"{urlsplit(url).hostname}: {message}"))
    for finding in findings:
        print(f"{finding.severity.upper()} {finding.path}:{finding.line} [{finding.rule}] {finding.message}")
    errors = sum(item.severity == "error" for item in findings)
    warnings = sum(item.severity == "warning" for item in findings)
    print(f"Checked {count} public files: {errors} errors, {warnings} advisory findings; {len(network)} external URLs checked.")
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps({"files": count, "errors": errors, "warnings": warnings,
            "findings": [asdict(item) for item in findings], "external": network}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())

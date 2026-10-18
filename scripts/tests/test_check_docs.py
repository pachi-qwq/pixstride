"""Regression cases for privacy, GFM boundaries, and bilingual bookkeeping."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import MagicMock, patch
from urllib.error import HTTPError

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_docs as docs


class MarkdownTests(unittest.TestCase):
    def test_fenced_code_is_not_prose_or_links(self):
        page = docs.parse_markdown('# Title\n\n```rust\n# Hidden\n[bad](absent.md)\nGithub\n```\n', 'a.md')
        self.assertEqual(page.links, [])
        self.assertEqual(page.findings, [])

    def test_longer_fence_preserves_inner_triples(self):
        page = docs.parse_markdown('# Title\n\n````text\n```\n# Hidden\n```\n````\n', 'a.md')
        self.assertEqual(page.findings, [])

    def test_tilde_fence(self):
        page = docs.parse_markdown('# Title\n\n~~~text\n[bad](missing.md)\n~~~\n', 'a.md')
        self.assertFalse(page.findings)
        self.assertFalse(page.links)

    def test_indented_code(self):
        page = docs.parse_markdown('# Title\n\n    [bad](missing.md)\n', 'a.md')
        self.assertFalse(page.links)

    def test_inline_code_and_api_identifiers(self):
        page = docs.parse_markdown('# Title\n\n`[bad](missing.md)` and `ID3D11DeviceContext::CopyResource`.\n', 'a.md')
        self.assertFalse(page.links)
        self.assertFalse(page.findings)

    def test_double_backtick_span(self):
        page = docs.parse_markdown('# Title\n\n``a ` [bad](missing.md) b``\n', 'a.md')
        self.assertFalse(page.links)

    def test_balanced_url_and_title(self):
        links, errors = docs.extract_links('[a](https://example.org/a_(b) "title (example)")')
        self.assertEqual(links[0].target, 'https://example.org/a_(b)')
        self.assertFalse(errors)

    def test_angle_destination_with_space(self):
        links, errors = docs.extract_links('[a](<guide name.md> "title")')
        self.assertEqual(links[0].target, 'guide name.md')
        self.assertFalse(errors)

    def test_escaped_parentheses(self):
        links, _ = docs.extract_links(r'[a](guide\(a\).md)')
        self.assertEqual(links[0].target, 'guide(a).md')

    def test_multiline_inline_link(self):
        links, errors = docs.extract_links('[a]\n(guide.md)')
        self.assertEqual((links[0].target, links[0].line), ('guide.md', 1))
        self.assertFalse(errors)

    def test_reference_collapsed_and_shortcut_links(self):
        links, errors = docs.extract_links('[A][key]\n[key][]\n[key]\n\n[key]: guide.md "Guide"\n')
        self.assertEqual(len(links), 4)
        self.assertTrue(all(link.target == 'guide.md' for link in links))
        self.assertFalse(errors)

    def test_missing_explicit_reference(self):
        _, errors = docs.extract_links('[a][missing]')
        self.assertEqual(len(errors), 1)

    def test_autolink(self):
        links, _ = docs.extract_links('<https://example.org/a>')
        self.assertEqual(links[0].target, 'https://example.org/a')

    def test_duplicate_and_chinese_anchors(self):
        page = docs.parse_markdown('# Title\n\n## Same\n\n## Same\n\n## 许可证决策\n', 'a.md')
        self.assertTrue({'same', 'same-1', '许可证决策'}.issubset(page.anchors))

    def test_html_comments_and_explicit_anchor(self):
        page = docs.parse_markdown('# Title\n\n<!-- [hidden](missing.md) -->\n\n<a id="named"></a>\n', 'a.md')
        self.assertFalse(page.links)
        self.assertIn('named', page.anchors)

    def test_structure_errors(self):
        page = docs.parse_markdown('# Title\n\n### Skipped\nText\n\n```\nx\n', 'a.md')
        rules = {f.rule for f in page.findings}
        self.assertTrue({'heading-level', 'heading-spacing', 'fence-language', 'fence-unclosed'} <= rules)

    def test_chinese_advice_is_nonblocking(self):
        page = docs.parse_markdown('# Title\n\n调用GPU,可以全面提升效果。\n', 'a.md')
        self.assertTrue(page.findings)
        self.assertTrue(all(f.severity == 'warning' for f in page.findings))

    def test_quoted_rewrite_is_not_advice(self):
        page = docs.parse_markdown('# Title\n\n> 调用GPU,可以全面提升效果。\n', 'a.md')
        self.assertFalse(page.findings)

    def test_legitimate_comparison_is_not_banned(self):
        page = docs.parse_markdown('# Title\n\n不是每个解码器都支持区域读取，而是需要逐一查询能力。\n', 'a.md')
        self.assertFalse(page.findings)

    def test_privacy_still_scans_fences_and_redacts(self):
        # Construct synthetic detector inputs without embedding real machine paths/keys.
        fake = 'C:' + chr(92) + 'Users' + chr(92) + 'example' + chr(92) + 'sample.png'
        findings = docs.scan_raw('# Title\n\n```text\n'+fake+'\n```\n', 'a.md')
        self.assertTrue(findings)
        self.assertTrue(all(fake not in f.message for f in findings))

    def test_synthetic_credential_in_comment_is_detected(self):
        fake = 'gh' + 'p_' + 'a' * 36
        findings = docs.scan_raw('<!-- '+fake+' -->', 'a.md')
        self.assertIn('github-token', {f.rule for f in findings})
        self.assertTrue(all(fake not in f.message for f in findings))

    def test_percent_units_and_api_names_are_allowed(self):
        page = docs.parse_markdown('# Title\n\n预算为 512 MiB，比例为 25%，调用 `IDXGISwapChain1::Present1`。\n', 'a.md')
        self.assertFalse(page.findings)


class TreeTests(unittest.TestCase):
    def setUp(self):
        parent = Path(os.environ.get('PIXSTRIDE_DOC_TEST_ROOT', tempfile.gettempdir())).resolve()
        self.temp = tempfile.TemporaryDirectory(prefix='pixstride-docs-', dir=parent)
        self.root = Path(self.temp.name).resolve()
        self.assertTrue(self.root.is_relative_to(parent))
        self.addCleanup(self.temp.cleanup)
        self.write('README.md', self.page('README.md', 'README.zh-CN.md', '# Title\n\nText.\n'))
        self.write('README.zh-CN.md', self.page('README.md', 'README.zh-CN.md', '# 标题\n\n正文。\n'))
        self.write('docs/document-pairs.json', json.dumps({'schema_version':1, 'pairs':[
            {'id':'readme','revision':1,'en':'README.md','zh-CN':'README.zh-CN.md'}], 'bilingual_templates':[]})+'\n')

    @staticmethod
    def page(en, zh, body):
        return f'[English]({en}) | [简体中文]({zh})\n\n<!-- doc-id: readme; revision: 1; status: active -->\n\n'+body

    def write(self, relative, text):
        file = self.root / relative
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text, encoding='utf-8', newline='\n')

    def rules(self):
        return {f.rule for f in docs.check_tree(self.root)[0] if f.severity == 'error'}

    def test_valid_tree(self):
        self.assertFalse(self.rules())

    def test_missing_fragment_and_case(self):
        file = self.root/'README.md'
        file.write_text(file.read_text(encoding='utf-8')+'\n[a](README.zh-CN.md#absent)\n[b](readme.zh-CN.md)\n', encoding='utf-8')
        self.assertTrue({'link-target','link-fragment'} <= self.rules())

    def test_local_chinese_fragment(self):
        file = self.root/'README.md'
        file.write_text(file.read_text(encoding='utf-8')+'\n[a](README.zh-CN.md#标题)\n', encoding='utf-8')
        self.assertNotIn('link-fragment', self.rules())

    def test_revision_and_literal_mismatch(self):
        file = self.root/'docs/document-pairs.json'
        data = json.loads(file.read_text(encoding='utf-8'))
        data['pairs'][0].update(revision=2, shared_literals=['NotPresent'])
        self.write('docs/document-pairs.json', json.dumps(data)+'\n')
        self.assertTrue({'pair-revision','shared-literal'} <= self.rules())

    def test_language_switch_must_point_to_pair(self):
        file = self.root/'README.md'
        text = file.read_text(encoding='utf-8').replace('[简体中文](README.zh-CN.md)', '[简体中文](README.md)')
        self.write('README.md', text)
        self.assertIn('language-entry', self.rules())

    def test_unpaired_page(self):
        self.write('extra.md', '# Extra\n')
        self.assertIn('unpaired-document', self.rules())

    def test_private_artifact_and_size(self):
        self.write('private/report.txt', 'Private.\n')
        self.write('large.txt', 'a'*(docs.MAX_BYTES+1))
        self.assertTrue({'private-artifact','file-size'} <= self.rules())

    def test_small_images_in_resource_directories(self):
        for name in ['assets/logo.png', 'tests/fixtures/small.gif']:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(b'\x89PNG\x00\xff')
        body = (self.root / 'README.md').read_text(encoding='utf-8')
        self.write('README.md', body + '\n![Logo](assets/logo.png)\n[Fixture](tests/fixtures/small.gif)\n')
        self.assertFalse(self.rules())

    def test_image_outside_resource_directories_is_rejected(self):
        (self.root / 'photo.png').write_bytes(b'\x89PNG\x00\xff')
        self.assertIn('private-artifact', self.rules())

    def test_resource_image_size_is_bounded(self):
        target = self.root / 'assets/huge.png'
        target.parent.mkdir()
        target.write_bytes(b'\x00' * (docs.MAX_BYTES + 1))
        self.assertIn('file-size', self.rules())

    def test_private_subdirectory_is_not_allowed_inside_assets(self):
        target = self.root / 'assets/private/photo.png'
        target.parent.mkdir(parents=True)
        target.write_bytes(b'\x00\xff')
        self.assertIn('private-artifact', self.rules())

    def test_private_test_outputs_are_rejected(self):
        self.write('tests/output/results.json', '{}\n')
        self.write('tests/local/sample.txt', 'Local.\n')
        self.assertIn('private-artifact', self.rules())

    def test_tracked_ignored_file_is_not_hidden(self):
        self.write('.gitignore', '/private/\n')
        self.write('private/report.txt', 'Private.\n')
        git = subprocess.CompletedProcess(
            args=['git', 'ls-files'],
            returncode=0,
            stdout=b'private/report.txt\0',
            stderr=b'',
        )
        with patch.object(docs.subprocess, 'run', return_value=git) as run_git:
            self.assertIn('private-artifact', self.rules())
        run_git.assert_called_once()

    def test_private_links(self):
        self.write('.idea/report.txt', 'Ignored.\n')
        file = self.root/'README.md'
        self.write('README.md', file.read_text(encoding='utf-8')+'\n[a](.idea/report.txt)\n')
        self.assertIn('link-private', self.rules())

    def test_outside_and_network_links(self):
        file = self.root/'README.md'
        self.write('README.md', file.read_text(encoding='utf-8')+'\n[a](../outside.md)\n[b](http://127.0.0.1/)\n')
        self.assertTrue({'link-target','private-host'} <= self.rules())


class NetworkTests(unittest.TestCase):
    def test_definite_missing_page_fails(self):
        with patch.object(docs, 'urlopen', side_effect=HTTPError('https://example.org',404,'missing',None,None)):
            self.assertEqual(docs.check_url('https://example.org')[0], 'error')

    def test_rate_limit_is_advisory(self):
        with patch.object(docs, 'urlopen', side_effect=HTTPError('https://example.org',429,'limited',None,None)):
            self.assertEqual(docs.check_url('https://example.org')[0], 'warning')

    def test_head_fallback_to_get(self):
        response = MagicMock()
        response.__enter__.return_value.status = 200
        with patch.object(docs, 'urlopen', side_effect=[HTTPError('https://example.org',405,'method',None,None), response]) as request:
            self.assertEqual(docs.check_url('https://example.org'), ('ok','200'))
            self.assertEqual(request.call_args_list[1].args[0].method, 'GET')

    def test_head_404_is_confirmed_with_get(self):
        response = MagicMock()
        response.__enter__.return_value.status = 200
        with patch.object(docs, 'urlopen', side_effect=[HTTPError('https://example.org',404,'head only',None,None), response]):
            self.assertEqual(docs.check_url('https://example.org'), ('ok','200'))


if __name__ == '__main__':
    unittest.main()

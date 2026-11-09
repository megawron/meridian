#!/usr/bin/env python3
"""
Regenerate po/meridian.pot from all files listed in po/POTFILES.in
following GNOME / GNU gettext conventions.
"""
import os
import re
import xml.etree.ElementTree as ET
from collections import OrderedDict

BASE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POTFILES_PATH = os.path.join(BASE_DIR, "po", "POTFILES.in")
POT_PATH = os.path.join(BASE_DIR, "po", "meridian.pot")


def unescape_rust_str(s: str) -> str:
    return (
        s.replace(r'\"', '"')
        .replace(r"\'", "'")
        .replace(r"\n", "\n")
        .replace(r"\r", "\r")
        .replace(r"\t", "\t")
        .replace(r"\\", "\\")
    )


def extract_strings():
    # key: (msgctxt_or_None, msgid) -> {"plural": msgid_plural_or_None, "locs": [loc, ...]}
    extracted = OrderedDict()

    def add(msgid, loc, msgctxt=None, msgid_plural=None):
        if not msgid:
            return
        key = (msgctxt, msgid)
        if key not in extracted:
            extracted[key] = {"plural": msgid_plural, "locs": []}
        elif msgid_plural and not extracted[key]["plural"]:
            extracted[key]["plural"] = msgid_plural
        if loc not in extracted[key]["locs"]:
            extracted[key]["locs"].append(loc)

    with open(POTFILES_PATH, "r", encoding="utf-8") as f:
        rel_files = [
            line.strip()
            for line in f
            if line.strip() and not line.strip().startswith("#")
        ]

    # Multi-line aware regexes for Rust gettext / tr! / N_ / ngettext / pgettext calls
    gettext_re = re.compile(
        r'(?:(?:crate::i18n::)?gettext|tr!|N_)\s*\(\s*"((?:[^"\\]|\\.)*)"\s*\)',
        re.DOTALL,
    )
    ngettext_re = re.compile(
        r'(?:crate::i18n::)?ngettext\s*\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*,',
        re.DOTALL,
    )
    pgettext_re = re.compile(
        r'(?:crate::i18n::)?pgettext\s*\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)"\s*\)',
        re.DOTALL,
    )
    action_contract_label_re = re.compile(r'\(\s*"([^"]+)"\s*,\s*"[a-z0-9\.\-]+"\s*\)')
    action_contract_title_re = re.compile(r'title:\s*"([^"]+)"')

    for rel_path in rel_files:
        full_path = os.path.join(BASE_DIR, rel_path)
        if not os.path.exists(full_path):
            continue

        if rel_path.endswith(".desktop.in"):
            with open(full_path, "r", encoding="utf-8") as f:
                for idx, line in enumerate(f, 1):
                    line_s = line.strip()
                    if not line_s or line_s.startswith("#") or line_s.startswith("["):
                        continue
                    if "=" in line_s:
                        k, v = line_s.split("=", 1)
                        if k in ("Name", "GenericName", "Comment", "Keywords") and v:
                            add(v, f"{rel_path}:{idx}")
            continue

        if rel_path.endswith(".metainfo.xml.in") or rel_path.endswith(".gschema.xml"):
            try:
                tree = ET.parse(full_path)
                root = tree.getroot()
                translatable_tags = {
                    "name",
                    "summary",
                    "description",
                    "p",
                    "li",
                    "caption",
                    "developer_name",
                }
                for elem in root.iter():
                    tag = elem.tag.split("}")[-1]
                    if tag in translatable_tags:
                        # Only leaf text or paragraph text without child block tags
                        children_tags = {c.tag.split("}")[-1] for c in elem}
                        if tag == "description" and (children_tags & {"p", "ul", "ol"}):
                            continue
                        text = " ".join((elem.text or "").split())
                        if text:
                            add(text, f"{rel_path}:1")
            except Exception as e:
                print(f"Warning: could not parse XML {rel_path}: {e}")
            continue

        with open(full_path, "r", encoding="utf-8") as f:
            content = f.read()

        # Precompute line offsets for accurate line numbers
        line_starts = [0]
        for m in re.finditer(r"\n", content):
            line_starts.append(m.end())

        def offset_to_line(offset: int) -> int:
            import bisect
            return bisect.bisect_right(line_starts, offset)

        for match in gettext_re.finditer(content):
            line_no = offset_to_line(match.start())
            add(unescape_rust_str(match.group(1)), f"{rel_path}:{line_no}")

        for match in ngettext_re.finditer(content):
            line_no = offset_to_line(match.start())
            s_sing = unescape_rust_str(match.group(1))
            s_plur = unescape_rust_str(match.group(2))
            add(s_sing, f"{rel_path}:{line_no}", msgid_plural=s_plur)

        for match in pgettext_re.finditer(content):
            line_no = offset_to_line(match.start())
            ctxt = unescape_rust_str(match.group(1))
            s_id = unescape_rust_str(match.group(2))
            add(s_id, f"{rel_path}:{line_no}", msgctxt=ctxt)

        if "action_contract.rs" in rel_path:
            for idx, line in enumerate(content.splitlines(), 1):
                loc = f"{rel_path}:{idx}"
                for match in action_contract_label_re.finditer(line):
                    add(match.group(1), loc)
                for match in action_contract_title_re.finditer(line):
                    add(match.group(1), loc)

    return extracted


def format_po_string(keyword: str, text: str) -> str:
    escaped = text.replace("\\", "\\\\").replace('"', r'\"')
    if "\n" in escaped:
        parts = escaped.split("\n")
        lines = [f'{keyword} ""']
        for i, p in enumerate(parts):
            if i < len(parts) - 1:
                lines.append(f'"{p}\\n"')
            elif p:
                lines.append(f'"{p}"')
        return "\n".join(lines)
    return f'{keyword} "{escaped}"'


def write_pot(extracted):
    lines = [
        "# SOME DESCRIPTIVE TITLE.",
        "# Copyright (C) 2026 Martin Gawron",
        "# This file is distributed under the same license as the meridian package.",
        "# FIRST AUTHOR <EMAIL@ADDRESS>, 2026.",
        "#",
        "#, fuzzy",
        'msgid ""',
        'msgstr ""',
        '"Project-Id-Version: meridian 0.1.0\\n"',
        '"Report-Msgid-Bugs-To: https://github.com/megawron/meridian/issues\\n"',
        '"POT-Creation-Date: 2026-11-09 12:00+0100\\n"',
        '"PO-Revision-Date: YEAR-MO-DA HO:MI+ZONE\\n"',
        '"Last-Translator: FULL NAME <EMAIL@ADDRESS>\\n"',
        '"Language-Team: LANGUAGE <LL@li.org>\\n"',
        '"Language: \\n"',
        '"MIME-Version: 1.0\\n"',
        '"Content-Type: text/plain; charset=UTF-8\\n"',
        '"Content-Transfer-Encoding: 8bit\\n"',
        '"Plural-Forms: nplurals=INTEGER; plural=EXPRESSION;\\n"',
        "",
    ]

    for (msgctxt, msgid), info in extracted.items():
        locs = info["locs"]
        msgid_plural = info["plural"]
        loc_str = " ".join(locs[:4])
        if len(locs) > 4:
            loc_str += f" and {len(locs) - 4} more"
        lines.append(f"#: {loc_str}")
        if msgctxt is not None:
            lines.append(format_po_string("msgctxt", msgctxt))
        lines.append(format_po_string("msgid", msgid))
        if msgid_plural is not None:
            lines.append(format_po_string("msgid_plural", msgid_plural))
            lines.append('msgstr[0] ""')
            lines.append('msgstr[1] ""')
        else:
            lines.append('msgstr ""')
        lines.append("")

    with open(POT_PATH, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"Wrote {len(extracted)} strings to {POT_PATH}")


if __name__ == "__main__":
    extracted = extract_strings()
    write_pot(extracted)

#!/usr/bin/env python3
import os
import re
import sys
import subprocess
from collections import OrderedDict

BASE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
POTFILES_PATH = os.path.join(BASE_DIR, "po", "POTFILES.in")
POT_PATH = os.path.join(BASE_DIR, "po", "evolution-gtk4.pot")
DE_PO_PATH = os.path.join(BASE_DIR, "po", "de.po")
MO_PATH = os.path.join(BASE_DIR, "data", "locale", "de", "LC_MESSAGES", "evolution-gtk4.mo")

def extract_strings():
    extracted = OrderedDict() # msgid -> list of "file:line"

    def add(msgid, loc):
        if not msgid:
            return
        # unescape escaped quotes if any, or normalize
        if msgid not in extracted:
            extracted[msgid] = []
        if loc not in extracted[msgid]:
            extracted[msgid].append(loc)

    with open(POTFILES_PATH, "r", encoding="utf-8") as f:
        rel_files = [line.strip() for line in f if line.strip() and not line.strip().startswith("#")]

    call_re = re.compile(r'(?:(?:crate::i18n::)?gettext|tr!|N_)\s*\(\s*"((?:[^"\\]|\\.)*)"\s*\)')
    action_contract_label_re = re.compile(r'\(\s*"([^"]+)"\s*,\s*"[a-z0-9\.\-]+"\s*\)')
    action_contract_title_re = re.compile(r'title:\s*"([^"]+)"')

    for rel_path in rel_files:
        full_path = os.path.join(BASE_DIR, rel_path)
        if not os.path.exists(full_path):
            continue
        with open(full_path, "r", encoding="utf-8") as f:
            lines = f.readlines()

        for idx, line in enumerate(lines, 1):
            loc = f"{rel_path}:{idx}"
            # Standard gettext/tr!/N_ calls
            for match in call_re.finditer(line):
                # Unescape \" -> "
                s = match.group(1).replace(r'\"', '"').replace(r'\n', '\n')
                add(s, loc)

            # In action_contract.rs, also match menu specs and shortcut titles
            if "action_contract.rs" in rel_path:
                for match in action_contract_label_re.finditer(line):
                    add(match.group(1), loc)
                for match in action_contract_title_re.finditer(line):
                    add(match.group(1), loc)

    return extracted

def write_pot(extracted):
    lines = [
        "# Translation template for evolution-gtk4.",
        "# Copyright (C) 2026 megawron",
        "# This file is distributed under the same license as the evolution-gtk4 package.",
        "# FIRST AUTHOR <EMAIL@ADDRESS>, 2026.",
        "#",
        "#, fuzzy",
        'msgid ""',
        'msgstr ""',
        '"Project-Id-Version: evolution-gtk4 0.1.0\\n"',
        '"Report-Msgid-Bugs-To: \\n"',
        '"POT-Creation-Date: 2026-09-20 02:40+0200\\n"',
        '"PO-Revision-Date: YEAR-MO-DA HO:MI+ZONE\\n"',
        '"Last-Translator: FULL NAME <EMAIL@ADDRESS>\\n"',
        '"Language-Team: LANGUAGE <LL@li.org>\\n"',
        '"Language: \\n"',
        '"MIME-Version: 1.0\\n"',
        '"Content-Type: text/plain; charset=UTF-8\\n"',
        '"Content-Transfer-Encoding: 8bit\\n"',
        ""
    ]

    for msgid, locs in extracted.items():
        # Location comments
        loc_str = " ".join(locs[:3])
        if len(locs) > 3:
            loc_str += f" and {len(locs)-3} more"
        lines.append(f"#: {loc_str}")
        escaped_id = msgid.replace('\\', '\\\\').replace('"', r'\"').replace('\n', '\\n"\n"')
        lines.append(f'msgid "{escaped_id}"')
        lines.append('msgstr ""')
        lines.append("")

    with open(POT_PATH, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"Wrote {len(extracted)} strings to {POT_PATH}")

if __name__ == "__main__":
    extracted = extract_strings()
    write_pot(extracted)

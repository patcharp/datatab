#!/usr/bin/env python3
"""Write release notes for a tag from its committed changelog."""

import argparse
from pathlib import Path
import re
import sys


INSTALL = ('Download the file for your system below, check it against `SHA256SUMS.txt`, '
           'and follow the steps on [datatab.pcplab.me/download]'
           '(https://datatab.pcplab.me/download/). Windows and Debian packages are unsigned. '
           'macOS releases use the self-signed PCPLab project certificate and are not notarized. '
           'Windows and macOS warn you before the first start.')


def release_notes(changelog, tag, repository):
    version = tag.removeprefix('v')
    lines = changelog.splitlines()
    heading = re.compile(r'^## \[([^]]+)\]')
    start = next((index for index, line in enumerate(lines)
                  if re.fullmatch(r'## \[' + re.escape(version) + r'\] - .+', line)), None)
    if start is None:
        raise ValueError(f'no changelog section for {version} (tag {tag})')
    end = next((index for index in range(start + 1, len(lines))
                if heading.match(lines[index])), len(lines))
    groups = {'new': [], 'Upgrading': [], 'Rollback': []}
    current = 'new'
    for line in lines[start + 1:end]:
        if line.startswith('### '):
            name = line[4:].strip()
            current = name if name in ('Upgrading', 'Rollback') else 'new'
            groups[current].append('## ' + name if current != 'new' else line)
        else:
            groups[current].append(line)
    whats_new = '\n'.join(groups['new']).strip()
    if not whats_new:
        raise ValueError(f"What's new is empty for {version} (tag {tag})")
    sections = [f'DataTab {version}', "## What's new", whats_new]
    sections.extend('\n'.join(groups[name]).strip() for name in ('Upgrading', 'Rollback')
                    if groups[name])
    sections.extend(['## Install\n\n' + INSTALL,
                     f'Full history: [CHANGELOG.md](https://github.com/{repository}/blob/{tag}/CHANGELOG.md)'])
    return '\n\n'.join(section for section in sections if section) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('changelog', type=Path)
    parser.add_argument('tag')
    parser.add_argument('repository')
    args = parser.parse_args()
    try:
        print(release_notes(args.changelog.read_text(encoding='utf-8'), args.tag, args.repository), end='')
    except (OSError, ValueError) as error:
        print(f'release notes: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())

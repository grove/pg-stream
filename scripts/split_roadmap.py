#!/usr/bin/env python3
"""
Split the old monolithic ROADMAP.md into per-version files in the roadmap/ directory.
Each section starting with '## v' becomes roadmap/vX.Y.Z.md-full.md
Then update ROADMAP.md to add a 'Full details' column.
"""

import re
import sys
from pathlib import Path

ROADMAP = Path("ROADMAP.md")
ROADMAP_DIR = Path("roadmap")


def version_slug(header: str) -> str:
    """Extract version slug from a section header like '## v0.17.0 — ...'"""
    m = re.match(r"^## (v[\d.x]+)", header)
    if m:
        return m.group(1)
    return None


def split_roadmap():
    lines = ROADMAP.read_text(encoding="utf-8").splitlines(keepends=True)

    # Find all ## v sections and their start lines
    sections = []  # list of (version_slug, start_line_idx)
    for i, line in enumerate(lines):
        slug = version_slug(line.strip())
        if slug:
            sections.append((slug, i))

    # Also find Post-1.0 sections (to know where last version ends)
    end_of_versions = len(lines)
    for i, line in enumerate(lines):
        if line.startswith("## Post-1.0"):
            end_of_versions = i
            break

    created = []
    for idx, (slug, start) in enumerate(sections):
        # Determine end: next version section, or end_of_versions
        if idx + 1 < len(sections):
            end = sections[idx + 1][1]
        else:
            end = end_of_versions

        section_lines = lines[start:end]

        # Strip trailing blank lines
        while section_lines and section_lines[-1].strip() == "":
            section_lines.pop()

        # Build file name: vX.Y.Z.md-full.md  (e.g. v0.17.0.md-full.md)
        filename = f"{slug}.md-full.md"
        out_path = ROADMAP_DIR / filename

        header_note = "> **See also:** [ROADMAP.md](../ROADMAP.md)\n\n"

        content = header_note + "".join(section_lines) + "\n"
        out_path.write_text(content, encoding="utf-8")
        created.append(filename)
        print(f"  created: roadmap/{filename}  ({end - start} lines)")

    return created


def update_readme(created_files):
    readme_path = Path("ROADMAP.md")
    text = readme_path.read_text(encoding="utf-8")

    # Build a lookup: version slug → filename
    slug_to_file = {}
    for fn in created_files:
        # fn is like 'v0.17.0.md-full.md'; slug is 'v0.17.0'
        slug = re.match(r"(v[\d.x]+)\.md-full\.md", fn).group(1)
        slug_to_file[slug] = fn

    # --- Add header column ---
    # Tables have format:
    #   | Version | Theme | Status | Scope |
    # We want:
    #   | Version | Theme | Status | Scope | Full details |
    # And corresponding separator row:
    #   |---------|-------|--------|-------|--------------|

    lines = text.splitlines(keepends=True)
    new_lines = []
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.rstrip("\n")

        # Detect table header row with exactly the columns we expect
        # Pattern: | Version | ... | Scope |
        if re.match(r"\|\s*Version\s*\|", stripped) and stripped.rstrip().endswith("|"):
            # This is a table header — add Full details column
            new_line = stripped.rstrip("|").rstrip() + " | Full details |\n"
            new_lines.append(new_line)
            i += 1
            # Next line should be separator
            if i < len(lines):
                sep = lines[i].rstrip("\n")
                # Add a separator cell
                new_sep = sep.rstrip("|").rstrip() + " |---------- |\n"
                new_lines.append(new_sep)
                i += 1
            continue

        # Detect table data rows: | [vX.Y.Z](vX.Y.Z.md) | ... |
        m = re.match(r"\|\s*\[v([\d.x]+)\]\(v[\d.x]+\.md\)\s*\|", stripped)
        if m:
            ver = "v" + m.group(1)
            # Find the full.md file for this version; if not found, try parent series
            full_file = slug_to_file.get(ver)
            if full_file is None:
                # e.g. v0.1.0 → look for v0.1.x
                parts = ver.split(".")
                series = f"{parts[0]}.{parts[1]}.x"
                full_file = slug_to_file.get(series)
            if full_file:
                link = f"[Full details]({full_file})"
            else:
                link = ""
            new_line = stripped.rstrip("|").rstrip() + f" | {link} |\n"
            new_lines.append(new_line)
            i += 1
            continue

        new_lines.append(line)
        i += 1

    readme_path.write_text("".join(new_lines), encoding="utf-8")
    print(f"\n  updated: ROADMAP.md")


if __name__ == "__main__":
    import os
    # Run from project root
    root = Path(__file__).parent.parent
    os.chdir(root)

    print("Splitting ROADMAP.md...")
    created = split_roadmap()
    print(f"\nCreated {len(created)} files.")

    print("\nUpdating ROADMAP.md...")
    update_readme(created)
    print("Done.")

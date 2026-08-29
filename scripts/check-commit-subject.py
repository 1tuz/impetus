#!/usr/bin/env python3
"""Validate the repository's Conventional Commit subject contract."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import sys


# New format: type: summary (closes #N) or type(scope): summary (refs #N)
SUBJECT_RE = re.compile(
    r"^(feat|fix|docs|refactor|perf|test|build|ci|chore|revert)"
    r"(\([a-z0-9_-]+\))?: "
    r"[a-z0-9].+"
    r"( \((closes|fixes|refs|references) #[0-9]+\))?$"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--message", help="subject to validate")
    source.add_argument("--file", type=Path, help="Git commit message file")
    return parser.parse_args()


def load_subject(args: argparse.Namespace) -> str:
    if args.file is not None:
        lines = args.file.read_text(encoding="utf-8").splitlines()
        return lines[0].strip() if lines else ""
    if args.message is not None:
        return args.message.strip()
    return os.environ.get("COMMIT_SUBJECT", "").strip()


def validation_error(subject: str) -> str | None:
    if not subject:
        return "subject is empty"
    if len(subject) > 72:
        return f"subject exceeds 72 characters: {len(subject)}"
    if subject.startswith(("WIP", "fixup!", "squash!")):
        return "WIP/fixup/squash commits are not allowed"

    match = SUBJECT_RE.fullmatch(subject)
    if match is None:
        return (
            "format must be: type: summary or type(scope): summary\n"
            "  - start with lowercase after colon\n"
            "  - optionally end with (closes #N) or (refs #N)"
        )

    # Check for ending punctuation (except closing paren from issue ref)
    if subject.rstrip(")").endswith((".", "!", "?", ";", ":")):
        return "subject must not end with punctuation"
    
    return None


def main() -> int:
    subject = load_subject(parse_args())
    error = validation_error(subject)
    if error is not None:
        print(f"Invalid commit subject: {error}", file=sys.stderr)
        return 1
    print(f"Commit subject accepted: {subject}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

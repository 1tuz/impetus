#!/usr/bin/env python3
"""Validate the repository's Conventional Commit subject contract."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import sys


SUBJECT_RE = re.compile(
    r"^[A-Z][A-Z0-9]+-[0-9]+ "
    r"(feat|fix|docs|refactor|perf|test|build|ci|chore|revert)(!)?: (.+)$"
)
INFINITIVE_RE = re.compile(
    r"^(Добавить|Изменить|Исправить|Обновить|Удалить|Реализовать|Создать|"
    r"Перенести|Настроить|Подключить|Проверить|Описать|Сделать)\b"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--message", help="subject to validate")
    source.add_argument("--file", type=Path, help="Git commit message file")
    return parser.parse_args()


def load_subject(args: argparse.Namespace) -> str:
    if args.file is not None:
        return args.file.read_text(encoding="utf-8").splitlines()[0].strip()
    if args.message is not None:
        return args.message.strip()
    return os.environ.get("COMMIT_SUBJECT", "").strip()


def validation_error(subject: str) -> str | None:
    if not subject:
        return "subject пуст"
    if len(subject) > 72:
        return f"subject длиннее 72 символов: {len(subject)}"
    if subject.startswith(("WIP", "fixup!", "squash!")):
        return "WIP/fixup/squash commit запрещён"

    match = SUBJECT_RE.fullmatch(subject)
    if match is None:
        return "нужен формат KEY-123 type: Результат"

    summary = match.group(3)
    if not (summary[0].isupper() or summary[0].isdigit()):
        return "описание после двоеточия начинается с заглавной буквы или цифры"
    if summary.endswith((".", "!", "?", ";", ":")):
        return "в конце subject не нужна пунктуация"
    if INFINITIVE_RE.match(summary):
        return "опиши результат, а не действие в инфинитиве"
    return None


def main() -> int:
    subject = load_subject(parse_args())
    error = validation_error(subject)
    if error is not None:
        print(f"Некорректный commit subject: {error}", file=sys.stderr)
        return 1
    print(f"Commit subject принят: {subject}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

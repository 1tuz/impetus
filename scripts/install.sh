#!/usr/bin/env zsh
set -euo pipefail

REPO="1tuz/impetus"
INSTALL_DIR="${IMPETUS_INSTALL_DIR:-$HOME/.local/bin}"
GITHUB_API="https://api.github.com/repos/$REPO/releases/latest"

error() {
  print -u2 "Ошибка: $*"
  exit 1
}

info() {
  print "$*"
}

detect_platform() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"
  
  case "$OS" in
    Darwin)
      if [[ "$ARCH" == "arm64" ]]; then
        ARTIFACT="impetus-macos-aarch64"
      else
        error "Поддерживается только macOS Apple Silicon (arm64)"
      fi
      ;;
    Linux)
      if [[ "$ARCH" == "x86_64" ]]; then
        ARTIFACT="impetus-linux-x86_64"
      else
        error "Поддерживается только Linux x86_64"
      fi
      ;;
    *)
      error "Неподдерживаемая платформа: $OS"
      ;;
  esac
  
  info "Платформа: $OS $ARCH ($ARTIFACT)"
}

check_deps() {
  command -v curl >/dev/null || error "Требуется curl"
  command -v tar >/dev/null || error "Требуется tar"
  command -v shasum >/dev/null || error "Требуется shasum"
}

fetch_release() {
  info "Получение последнего релиза..."
  RELEASE_DATA=$(curl -sSfL "$GITHUB_API") || error "Не удалось получить данные релиза"
  
  DOWNLOAD_URL=$(print "$RELEASE_DATA" | grep -o "\"browser_download_url\": \"[^\"]*${ARTIFACT}.tar.gz\"" | cut -d'"' -f4)
  CHECKSUM_URL=$(print "$RELEASE_DATA" | grep -o "\"browser_download_url\": \"[^\"]*${ARTIFACT}.tar.gz.sha256\"" | cut -d'"' -f4)
  
  [[ -n "$DOWNLOAD_URL" ]] || error "Не найден архив для $ARTIFACT"
  [[ -n "$CHECKSUM_URL" ]] || error "Не найдена контрольная сумма"
}

download_and_verify() {
  TMPDIR=$(mktemp -d)
  trap "rm -rf '$TMPDIR'" EXIT
  
  info "Скачивание $DOWNLOAD_URL..."
  curl -sSfL -o "$TMPDIR/impetus.tar.gz" "$DOWNLOAD_URL" || error "Не удалось скачать архив"
  
  info "Скачивание контрольной суммы..."
  curl -sSfL -o "$TMPDIR/impetus.tar.gz.sha256" "$CHECKSUM_URL" || error "Не удалось скачать контрольную сумму"
  
  info "Проверка контрольной суммы..."
  if command -v shasum >/dev/null; then
    (cd "$TMPDIR" && shasum -a 256 -c impetus.tar.gz.sha256) || error "Контрольная сумма не совпала"
  else
    (cd "$TMPDIR" && sha256sum -c impetus.tar.gz.sha256) || error "Контрольная сумма не совпала"
  fi
  
  info "Распаковка..."
  tar xzf "$TMPDIR/impetus.tar.gz" -C "$TMPDIR" || error "Не удалось распаковать архив"
}

install_binaries() {
  mkdir -p "$INSTALL_DIR"
  
  info "Установка в $INSTALL_DIR..."
  mv "$TMPDIR/impetus" "$INSTALL_DIR/impetus"
  mv "$TMPDIR/impetusd" "$INSTALL_DIR/impetusd"
  
  chmod +x "$INSTALL_DIR/impetus" "$INSTALL_DIR/impetusd"
}

show_post_install() {
  info ""
  info "✓ Установка завершена!"
  info ""
  info "Добавьте $INSTALL_DIR в PATH, если ещё не добавлен:"
  info "  export PATH=\"\$HOME/.local/bin:\$PATH\""
  info ""
  info "Запуск:"
  info "  impetusd       # запустить daemon"
  info "  impetus        # CLI клиент"
  info ""
  info "Документация: https://github.com/$REPO"
}

main() {
  detect_platform
  check_deps
  fetch_release
  download_and_verify
  install_binaries
  show_post_install
}

main

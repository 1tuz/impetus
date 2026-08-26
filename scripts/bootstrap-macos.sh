#!/usr/bin/env zsh
set -euo pipefail

if ! command -v xcodebuild >/dev/null; then
  print -u2 'Нужен Xcode: установьте его из App Store, откройте один раз и запустите xcode-select --install.'
  exit 1
fi

if ! xcrun -sdk macosx -find metal >/dev/null 2>&1; then
  print -u2 'В Xcode не найден Metal compiler. Установите Metal Toolchain: xcodebuild -downloadComponent MetalToolchain.'
  exit 1
fi

if ! command -v rustup >/dev/null; then
  print -u2 'Нужен rustup: https://rustup.rs/'
  exit 1
fi

rustup toolchain install 1.98.0 --profile minimal --component clippy --component rustfmt
print 'macOS prerequisites проверены: Xcode/Metal и Rust 1.98.0 готовы.'

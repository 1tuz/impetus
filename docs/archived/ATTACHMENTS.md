# Скриншоты и файлы как model context

## Принцип

Attachment — не строка prompt и не разрешение агенту читать произвольный путь.
Человек явно выбирает файл/скриншот, видит preview и target backend, после чего
runtime создаёт типизированную ссылку на локальный blob. SQLite events содержат
metadata/reference, но не сырые bytes.

```text
Native picker / explicit screen capture
  → scope + type + size validation
  → local immutable blob + SHA-256
  → metadata/secret/prompt-injection scan
  → user preview and exact-recipient confirmation
  → provider/ACP capability negotiation
  → one prompt delivery
```

Фоновый захват экрана, автоматическая отправка найденного файла и скрытая повторная
отправка attachment в другой backend запрещены.

## Модель данных

```text
Attachment
  id, session_id
  source: user_selected | user_screenshot
  kind: image | text | document | binary
  display_name, detected_media_type, byte_len, sha256
  local_blob_ref
  scan_state, redaction_state
  created_at, deleted_at?

PromptAttachmentRef
  attachment_id, prompt_id, backend_profile_id
  transform_id, provider_upload_ref?, delivery_state
```

`local_blob_ref` и `provider_upload_ref` — opaque references. Base64, PDF/text
contents, EXIF, API response и provider file ID не дублируются в event body.
Локальный blob удаляется вместе с сессией либо по явной команде пользователя;
provider deletion/retention показываются отдельно, потому что зависят от backend.

## Проверки до отправки

- native macOS picker выдаёт security-scoped selection; directory и symlink за
  разрешённый scope не разворачиваются автоматически;
- MIME определяется по содержимому, а не только по extension;
- executable, device, socket и неизвестный binary по умолчанию отклоняются;
- архив не распаковывается автоматически; directory upload требует отдельного RFC;
- действуют per-file, per-prompt и per-session byte limits;
- transmitted image strip-ит EXIF/GPS и получает отдельный transform hash;
- original остаётся локальным; preview показывает именно отправляемую копию;
- secret scan и outbound policy выполняются до provider upload;
- содержимое считается недоверенным и проходит input probe до model context.

Начальные консервативные лимиты должны быть закреплены benchmark/RFC после spike,
а не спрятаны в UI. Oversize показывает точный размер и безопасные варианты:
выбрать страницы PDF, уменьшить изображение или отправить path/resource link
backend-у, который уже имеет локальный доступ.

## Скриншоты

- capture запускается только явным действием человека: screen, window или region;
- macOS Screen Recording permission остаётся системным permission, приложение его
  не имитирует и не обходит;
- перед отправкой видны thumbnail, pixel dimensions, размер и backend;
- можно удалить/замазать области до отправки; передаваемая производная получает
  новый hash;
- `Safe Auto` не может сам снять экран или добавить screenshot в prompt.

## ACP и direct providers

ACP v1 уже определяет `ContentBlock` для image, embedded resource и resource link.
Image в prompt разрешён только при negotiated `image` capability; embedded content
требует `embeddedContext`. Поэтому adapter:

1. сохраняет capability snapshot при старте ACP session;
2. не отправляет unsupported block и не превращает его молча в base64-текст;
3. для локального ACP agent предпочитает `resource_link`, только если agent уже
   имеет разрешённый доступ к этому URI;
4. для внешнего backend использует embedded content/provider upload лишь после
   outbound approval и adapter-specific limits;
5. показывает `Unsupported by selected model`, если active model text-only.

Direct provider adapter выбирает транспорт сам: image block, provider file upload
или sandbox-mounted read-only file. Provider upload ID — не локальный blob и не
доказательство удаления у провайдера.

## UX composer

Рядом с composer находятся `Attach file` и `Capture`. Каждый attachment — chip с
именем, типом, размером, scan state и remove action. Send недоступен, пока scan не
завершён или backend не подтвердил input capability. Перед первым external upload
показываются exact backend/organization, список файлов и redaction result.

Drag-and-drop и paste image эквивалентны picker: они создают preview, но ничего не
отправляют. `@path` означает ссылку на workspace resource и проходит отдельную
read policy; это не синоним upload.

## Этапы и тесты

- v0.3: typed attachment refs, composer chips, ACP capability negotiation, image
  prompt для mock agent и unsupported-state;
- v0.4: durable local blob lifecycle, deletion/export и bounded history context;
- v0.5: outbound policy, redaction/scan pipeline и provider upload adapter;
- screen capture — после file attachment vertical slice и отдельного macOS
  permission/privacy spike.

Обязательные тесты: magic-byte mismatch, symlink escape, EXIF stripping, secret
fixture, oversize, unsupported model, provider failure/retry without duplicate
upload, session deletion, Auto mode attempting an implicit attachment, and proof
that raw bytes are absent from SQLite/export/tracing.

## Первичные источники

- [ACP v1 content blocks and prompt capabilities](https://agentclientprotocol.com/protocol/v1/content)
- [ACP v1 schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/schema/v1/schema.json)
- [Claude Code Desktop file attachments](https://code.claude.com/docs/en/desktop)
- [Anthropic Vision API](https://platform.claude.com/docs/en/build-with-claude/vision)

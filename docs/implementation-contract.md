# Рабочий контракт реализации блока 1

Версия 1, 2026-09-13. Реализация разрешена владельцем. Этапы принимаются последовательно; внутри этапа API-контракт позволяет независимую работу backend и frontend.

## Среда

Локальный пилот: API Rust `127.0.0.1:18480`, Vite `127.0.0.1:15173`, отдельный PostgreSQL на `127.0.0.1:58432`, compose project `otdel-block1`. Существующие контейнеры других проектов не менять. Оригиналы локально в игнорируемом каталоге с объектными ключами; хранилище скрыто за интерфейсом, не раздаётся как статическая папка.

Пользователь предоставит OpenRouter-ключ позже. До этого реальные ИИ-вызовы не подменяются тестовыми ответами. Продуктовые роли используют адаптер OpenRouter, Claude Code остаётся исполнителем разработки. Ключ задаётся локальной переменной окружения, не хранится в браузере/публичном Git.

## API этапа 1A

Префикс `/api`. Поля snake_case; UUID строками, время RFC3339 UTC. Ошибка: `{ "error": { "code": "...", "message": "...", "retryable": false } }`. Списки: `{ "items": [...] }`.

- `GET /health` — liveness; `GET /ready` — соединение с БД и хранилищем.
- `POST /api/session` `{password}` → `{authenticated:true, csrf_token}` + HttpOnly SameSite=Strict session cookie.
- `GET /api/session` → `{authenticated:true, csrf_token}` либо 401. `DELETE /api/session` завершает сессию.
- Изменяющие запросы после входа требуют `X-CSRF-Token`. CORS только явно заданному локальному frontend origin с credentials. Секрет/пароль не логируется.
- `GET /api/partners` → список; `POST /api/partners` `{name, note?}` → Partner с 201.
- `GET /api/partners/{id}` → Partner; `PATCH` допускает `{name?,note?}`.
- Partner: `{id,name,note,created_at,updated_at}`. Имя 1–200 символов после trim, note до 10000.
- `GET /api/partners/{id}/materials` → список Material.
- `POST /api/partners/{id}/materials` multipart `file` (один файл/запрос) → Material, 201 для нового, 200 для точного дубля. До 25 MiB на файл, PDF/PNG/JPEG на 1A. Upload потоковый, проверка сигнатуры, basename имени не используется как путь.
- Material: `{id,partner_id,filename,media_type,size_bytes,sha256,status,page_count,created_at,error}`. status на 1A: `queued`, `processing`, `completed`, `partial`, `failed`, `quarantined`; новый файл ожидает извлечения, не называется прочитанным. page_count nullable.
- `GET /api/partners/{id}/materials/{material_id}/original` — авторизованная отдача файла только из этого партнёра; корректные Content-Type/Disposition, nosniff.
- `POST /api/partners/{id}/materials/{material_id}/retry` → Material. Повтор только для подходящего статуса, идемпотентность задания.
- `GET /api/partners/{id}/jobs` → список Job `{id,partner_id,material_id,kind,status,stage,attempts,created_at,updated_at,error}`.

Фронтенд отправляет `credentials:include`; токен CSRF хранит в памяти, восстанавливает через GET session. Пароль вводится на экране входа, не сохраняется клиентом. Настоящая карточка и выбранный партнёр восстанавливаются после перезагрузки (URL содержит partner_id).

## Хранение и безопасность 1A

Пароли/секреты задаются при локальной инициализации случайно; приложение отказывается стартовать с пустым/примерным секретом. Сессии непрозрачные, с ограниченным сроком, сервер хранит хеш токена. Один владелец локального пилота, не открытая регистрация.

Таблицы привязаны к workspace/bureau, scoped SQL и RLS на отдельной runtime-роли (не владелец таблиц/BYPASSRLS). Миграции идут отдельно. Конкретный partner_id проверяется в каждом доступе к материалу/задаче. Тесты отдельно проверяют чужой workspace и несовпадение partner/material.

Дедупликация по партнёру+sha256. Новый изменённый файл — новая запись/версия, оригинал не перезаписывается. Запись объекта выполняется до атомарной публикации метаданных/задания; неудача оставляет видимый исход и контролируемую очистку временного файла. API никогда не принимает произвольный локальный путь.

Очередь содержит идемпотентный ключ, попытки, lease, время следующего запуска. На 1A не запускать фиктивное извлечение: worker этапа 1B будет обрабатывать сохранённые задания.

## API этапа 1B — страницы, области источников и таблицы

Те же правила конверта, авторизации и CSRF, что и в 1A. Дополнения к 1A строго аддитивные.

**Изменения существующих объектов.**

- `Material` получает поле `extraction`: `null`, пока материал не читали, иначе объект
  `{pages_total, pages_extracted, pages_empty, pages_needs_ocr, pages_partial, pages_failed,
  pages_pending, parser_name, parser_version, ocr_engine, ocr_version, started_at, finished_at,
  diagnostic}`. Счётчики вычисляются из строк страниц при чтении, а не хранятся отдельно.
- `Job` получает `page_number`: `null` для задания на весь документ, номер страницы — для
  повторного чтения одной страницы. `kind` теперь `extract_document` либо `extract_page`.
- `Material.status` больше не задаётся напрямую: он выводится из исходов страниц.
  `completed` означает «все страницы завершены чисто», `partial` — «часть страниц не
  прочитана», `failed` — «пригодного содержимого не получено».

**Новые эндпоинты.**

- `GET /api/partners/{id}/materials/{material_id}` → `Material` (со сводкой).
- `GET /api/partners/{id}/materials/{material_id}/pages` → `{items: [Page]}` в порядке
  страниц, без текста страниц.
- `GET /api/partners/{id}/materials/{material_id}/pages/{page_number}` →
  `{page, text, regions}`. `text` — `null`, если пригодного текста нет.
- `POST /api/partners/{id}/materials/{material_id}/pages/{page_number}/retry` → `Page`.
  Допустим только для `pending`, `needs_ocr`, `partial`, `failed`; иначе 409 `conflict`.
  Идемпотентен: повторное нажатие переиспользует ту же строку очереди.

`Page`: `{id, material_id, page_number, status, text_source, char_count, word_count,
image_count, width_pt, height_pt, rotation, parser_name, parser_version, ocr_engine,
ocr_version, ocr_language, duration_ms, attempts, diagnostic, extracted_at, region_count,
table_count}`.

- `status`: `pending` | `extracted` | `empty` | `needs_ocr` | `partial` | `failed`.
  `needs_ocr` — «текстового слоя нет, распознавание не выполнено»; это **не** успех и
  **не** пустая страница. `empty` — только для страницы без текста, изображений и графики.
- `text_source`: `none` | `text_layer` | `ocr`. `ocr` возможен только если движок
  действительно назван в `ocr_engine`.
- `diagnostic` — причина текущего статуса словами; интерфейс показывает её как есть.

`Region`: `{id, page_id, page_number, ordinal, kind, text, source, bbox, row_count,
column_count, cells}`. `kind`: `heading` | `paragraph` | `footnote` | `table`.
`bbox` — `{x0,y0,x1,y1}` в пользовательском пространстве PDF (начало внизу слева, пункты)
либо `null`, если адаптер не знает координат; выдуманный прямоугольник не возвращается.
`row_count`/`column_count` заполнены только у таблицы.

`Cell`: `{id, region_id, row_index, column_index, is_header, raw_text, value_kind, unit,
column_header, bbox}`. `raw_text` — дословный фрагмент источника; числовое поле не
предусмотрено намеренно. `value_kind`: `empty` | `number` | `text`; пустая ячейка остаётся
`empty` и никогда не становится нулём. `unit` заполняется, только если единица буквально
написана в ячейке или в заголовке её столбца (`column_header`, тоже дословный).

Страница-первоисточник открывается существующим `.../original` с фрагментом `#page=N`.
Снимки страниц в 1B не сохраняются: рендер выполняется только на время распознавания.

## Следующие контракты

1C — продукты/факты/глоссарий/пробелы; 1D — исследования и бюджеты; 1E — опубликованные версии и поиск/ответы; 1F — обновления и приёмку. Каждый контракт фиксируется до соответствующей UI-интеграции. Не создавать работающие на вид заглушки этих разделов раньше времени.

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

## API этапа 1C — продукты, факты с источниками, глоссарий, Q&A и пробелы

Те же правила конверта, авторизации и CSRF. Дополнения строго аддитивные.

**Состояние модели.** До получения ключа OpenRouter продуктолог не запускается; это
состояние настройки, а не ошибка материала.

- `GET /api/knowledge/provider` → `{state, provider, model, endpoint_host, missing[], message}`.
  `state`: `ready` | `needs_configuration` | `disabled`. `missing` перечисляет имена
  переменных окружения (`OTDEL_LLM_API_KEY`, …). Ключ не возвращается никогда и ни в
  каком виде; `endpoint_host` — только хост.

**Чтение черновика.** Все ответы ограничены партнёром внутри бюро.

- `GET /api/partners/{id}/knowledge` → `{provider, summary, runs: [KnowledgeRun],
  pending_materials: [{material_id, filename, pages_with_text}]}`. `pending_materials` —
  прочитанные материалы, разбор которых ещё ни разу не ставился в очередь (интерфейс
  предлагает по ним первый разбор).
- `GET /api/partners/{id}/knowledge/products` → `{items: [ProductNode]}`;
  `ProductNode` = `{product|null, category|null, facts: [Fact]}`. `product = null` —
  узел фактов о предложении в целом.
- `GET /api/partners/{id}/knowledge/glossary` → `{items: [Term]}`.
- `GET /api/partners/{id}/knowledge/qa` → `{items: [QaEntry]}`.
- `GET /api/partners/{id}/knowledge/gaps` → `{items: [Gap]}` (только открытые).

**Запуск разбора.**

- `POST /api/partners/{id}/materials/{material_id}/understand` → `KnowledgeRun`.
  Идемпотентен: пока разбор `queued`/`running`, повтор возвращает тот же запуск и не
  создаёт второе задание. 409 `conflict` — материал ещё не прочитан либо провайдер не
  настроен (сообщение называет недостающие переменные, `retryable: true`).

`KnowledgeRun`: `{id, partner_id, material_id, material_filename, status, provider,
model, prompt_profile, pages_considered, requests_made, input_chars,
categories_created, products_created, facts_accepted, facts_rejected, terms_created,
qa_created, gaps_created, questions_created, rejections[], diagnostic, started_at,
finished_at, created_at}`.

- `status`: `queued` | `running` | `completed` | `partial` | `failed` | `needs_provider`.
  `partial` — часть предложений модели отклонена или часть страниц не вошла в лимит;
  `needs_provider` — модель не настроена, обращений не было, ничего не сохранено.
- Счётчики созданного вычисляются из сохранённых строк при чтении, а не хранятся
  отдельно. `rejections` — причины отказов словами, интерфейс показывает их как есть.
- Один материал — одна строка запуска (текущее состояние разбора). История попыток
  остаётся на задании (`attempts`, `error`, `error_kind`).

`Fact`: `{id, partner_id, material_id, run_id, product_id, product_name, kind, status,
attribute, value_text, unit, conditions, model_context, evidence: [Evidence],
created_at}`.

- `kind`: `characteristic` | `limitation` | `application` | `commercial`.
- `status` на 1C всегда `candidate`. Статусы проверки (`source_supported` и др.) —
  этап 1E, и записать их здесь нельзя.
- `value_text` — значение дословно из источника; числового поля намеренно нет. Факт
  принимается, только если значение найдено в цитате как отдельный токен (`5` не
  подтверждается пятёркой внутри `1500`, `3.5` — внутри `13.5`). `unit` заполняется,
  только если единица буквально присутствует **в цитате** (не в значении, написанном
  моделью); `conditions` — только если условия найдены дословно в цитате, иначе текст
  переносится в `model_context`. Свойство (`attribute`) — это название характеристики
  по формулировке модели, дословного совпадения с источником для него не требуется.
- `model_context` — формулировка модели, **не цитата**; интерфейс помечает её отдельно.
- `evidence` никогда не пуст: факт без источника не сохраняется (отложенный триггер БД).

`Evidence`: `{id, material_id, material_filename, page_id, page_number, region_id,
quote, char_start, char_end}`. `quote` — дословный фрагмент сохранённого текста
страницы (сервер извлекает его по смещениям, а не сохраняет формулировку модели);
`char_start`/`char_end` — смещения в символах в `material_pages.text_content`.
Оригинал открывается существующим `.../original#page=N`.

`Term`: `{id, …, term, definition, definition_is_model_context, evidence[]}` —
определение, написанное моделью, помечено и не выдаётся за цитату.
`QaEntry`: `{id, …, question, answer, answer_is_model_context, evidence[]}` — ответ
является формулировкой модели, если он не найден дословно в источнике; интерфейс
помечает это и не выдаёт ответ за цитату.
`Gap`: `{id, …, product_id, product_name, topic, missing, blocks, question|null}`;
`question` = `{id, audience: partner|industry, text, status}` со статусом `prepared` —
на этом этапе вопросы не отправляются.

`Job.kind` дополняется значением `understand_material` (без `page_number`).

## API этапа 1D — ограниченное отраслевое исследование, бюджет и источники

Те же правила конверта, авторизации и CSRF. Дополнения строго аддитивные.

**Состояние адаптеров.** Поисковый провайдер для OTDEL не выбран (`block-01-spec.md` §3),
поэтому нормальное состояние — `needs_configuration`. Готовность требует всех трёх
половин: поискового endpoint, списка разрешённых хостов и модели. Исследователь, который
умеет искать, но не имеет права ничего прочитать (или прочитал бы, но не может
истолковать), израсходовал бы бюджет и не дал ответа.

- `GET /api/research/provider` → `{state, search, fetcher, model, missing[], allowed_hosts[],
  limits, message}`. `state`: `ready` | `needs_configuration` | `disabled`.
  `search`/`fetcher`/`model` — `AdapterView` = `{state, provider, endpoint_host, model,
  message}`. Ключ не возвращается никогда и ни в каком виде; `endpoint_host` — только хост.
  `limits` = `{max_queries_per_plan, max_results_per_query, max_sources_per_plan,
  max_page_bytes, max_page_chars, request_timeout_seconds, plan_time_budget_seconds,
  max_passes_per_plan}` — границы объявляются интерфейсу до запуска, а не после.

**Бюджет.** Суммы — целые, в миллионных долях валютной единицы; пересчёта между валютами
нет. Потолки — это настройка (`OTDEL_RESEARCH_BUDGET_MICROS`), балансы — данные.

- `GET /api/research/budget` → `ResearchBudget` = `{currency, limit_micros, reserved_micros,
  spent_micros, unknown_micros, available_micros, plan_budget_micros,
  cost_per_search_micros, cost_per_fetch_micros, cost_per_model_call_micros, updated_at}`.
  Тарифицируются все три вида внешнего вызова: поиск, загрузка страницы и обращение к
  модели при истолковании собранных источников.
  `reserved_micros` — деньги, удержанные под вызовы, которые сейчас выполняются;
  `unknown_micros` — часть `spent_micros` с неизвестным исходом, требующая сверки
  (`block-01-spec.md` §10). `available_micros = limit − spent − reserved`, не меньше нуля.
  Суммы посчитаны **по объявленному тарифу**, а не по счёту провайдера.

**Чтение.** Все ответы ограничены партнёром внутри бюро.

- `GET /api/partners/{id}/research` → `{provider, budget, summary, plans: [ResearchPlan],
  questions: [IndustryQuestion]}`.
- `GET /api/partners/{id}/research/findings?plan_id=` → `{items: [ResearchFinding]}`.
- `GET /api/partners/{id}/research/plans/{plan_id}/sources` → `{items: [ResearchSource]}`.
- `GET /api/partners/{id}/research/plans/{plan_id}/queries` → `{items: [ResearchQuery]}`.

**Запуск и остановка.**

- `POST /api/partners/{id}/research/questions/{question_id}/plan` → `ResearchPlan`.
  Единственный способ создать исследование: вопрос должен существовать в 1C с
  `audience = industry`. Идемпотентен — пока план `queued`/`running`, повтор возвращает
  тот же план; завершённый план ставится в очередь заново («исследовать заново»), в
  пределах `max_passes`. 409 `conflict`: адаптеры не настроены (`retryable: true`), бюджет
  бюро исчерпан, либо предел проходов исчерпан.
- `POST /api/partners/{id}/research/plans/{plan_id}/stop` → `ResearchPlan`. Ставит флаг;
  worker завершает план на ближайшей контрольной точке — всегда **до** платного вызова,
  поэтому остановка не оставляет наполовину списанных денег. 409 `conflict`, если план уже
  завершён.

`ResearchPlan`: `{id, partner_id, material_id, material_filename, question_id,
question_text, topic, status, provider, model, prompt_profile, passes, max_passes,
budget_micros, reserved_micros, spent_micros, queries_made, results_seen, sources_fetched,
sources_skipped, bytes_fetched, findings_accepted, findings_rejected, duration_ms,
rejections[], diagnostic, cancel_requested, started_at, finished_at, created_at,
updated_at}`.

- `status`: `queued` | `running` | `completed` | `partial` | `failed` | `needs_provider` |
  `budget_exhausted` | `cancelled` (словарь `block-01-spec.md` §7 плюс `needs_provider`).
  `needs_provider` — обращений не было и бюджет не резервировался; `budget_exhausted` —
  деньги кончились, работа остановлена, а не продолжена.
- `question_id` становится `null`, если 1C заново разобрал этот материал: исследование и
  его журнал сохраняются, а `question_text` — то, что действительно исследовалось.
- `rejections` — причины словами; интерфейс показывает их как есть.

`IndustryQuestion`: `{id, partner_id, material_id, material_filename, gap_id, gap_topic,
gap_missing, text, status, plan_id, created_at}`. `plan_id = null` — вопрос ждёт решения
владельца, и до решения с ним ничего не происходит.

`ResearchQuery`: `{id, plan_id, ordinal, query_text, provider, results_count, cost_micros,
outcome, diagnostic, created_at}`. `query_text` — дословно то, что было отправлено.
`outcome`: `ok` | `failed` | `unknown` | `refused`. `refused` — запрос не отправлялся
(например, вопрос называет партнёра) и стоил ноль; `unknown` — запрос ушёл, ответ не
получен: расход засчитан и помечен к сверке.

`ResearchSource`: `{id, plan_id, query_id, url, host, title, snippet, status, http_status,
content_type, content_bytes, content_chars, content_hash, license, license_note,
retrieved_at, published_at, cost_micros, diagnostic, created_at}`.

- `status`: `discovered` | `skipped_host` | `skipped_robots` | `skipped_limit` |
  `skipped_type` | `fetched` | `failed`. Любое значение кроме `fetched` означает, что
  содержимое **не получено**, и каждое называет свою причину. Строка существует и для
  непрочитанной ссылки: журнал, который её тихо выбрасывает, заставляет думать, что поиск
  ничего не нашёл.
- `snippet` — текст поисковика, средство обнаружения; цитировать его нельзя и ни одно
  доказательство на него не ссылается (`block-01-spec.md` §6.5).
- `license` заполняется, только если страница сама объявляет лицензию. `null` означает «не
  объявлена», а не «свободно»; `license_note` говорит, что именно.
- Текст снимка (`text_content`) через API не отдаётся: цитаты показываются в выводах, а
  выдача целых страниц превратила бы обзор плана в мегабайты.

`ResearchFinding`: `{id, partner_id, plan_id, scope, status, topic, attribute, value_text,
unit, conditions, model_context, evidence: [ExternalEvidence], created_at}`.

- `scope` всегда `industry`. Полей продукта, материала или артикула партнёра в контракте
  нет вовсе: отраслевой вывод не может стать характеристикой изделия партнёра, потому что
  его нечем так записать (`block-01-plan.md`, 1D §4).
- `status` на 1D всегда `candidate`. Статусы проверки — этап 1E.
- `value_text` — значение дословно из источника; вывод принимается, только если значение
  найдено в цитате как отдельный токен. `unit` — только если единица буквально есть **в
  цитате**; `conditions` — только если условия найдены в цитате, иначе текст переносится в
  `model_context`.
- `model_context` — формулировка модели, **не цитата**; интерфейс помечает её отдельно.
- `evidence` никогда не пуст: вывод без внешнего источника не сохраняется (отложенный
  триггер БД).

`ExternalEvidence`: `{id, source_id, url, host, retrieved_at, content_hash, license, quote,
char_start, char_end}`. `quote` — дословный фрагмент сохранённого снимка страницы (сервер
извлекает его по смещениям, а не сохраняет формулировку модели); `char_start`/`char_end` —
смещения в символах в `research_sources.text_content`. `retrieved_at` и `content_hash`
говорят, **когда** и **что именно** было прочитано.

`Job.kind` дополняется значением `research_plan`. План задания хранится отдельной колонкой
`jobs.research_plan_id` и в wire-контракт `Job` не добавляется: этапам 1A–1C он не нужен.

## API этапа 1E — проверка, версии знаний, публикация, поиск и ответы

Те же правила конверта, авторизации и CSRF. Дополнения строго аддитивные.

**Что здесь решается.** Кандидаты 1C и 1D проходят детерминированные правила, получают
статус проверки и либо попадают в **неизменяемую версию знаний**, либо честно остаются
неопубликованными. Искать и спрашивать можно **только по опубликованной версии**:
черновик 1C/1D через эти эндпоинты не виден вовсе.

**Состояние адаптеров.** Проверка и публикация **не требуют модели**: правила
детерминированы (`block-01-spec.md` §6.7 — «совпадение ответов двух моделей не является
доказательством»). Модель и embeddings нужны только двум необязательным надстройкам:
прозаическому ответу и векторной половине поиска. Без них система работает и говорит, в
каком именно режиме.

- `GET /api/retrieval/provider` → `{state, validation, embedding, answer, vector, search_mode,
  missing[], limits, message}`.
  - `validation` = `{mode: "deterministic", message}` — всегда доступна; поля `state` у неё
    нет намеренно, потому что выключить её нельзя.
  - `embedding`/`answer` — `AdapterView` = `{state, provider, endpoint_host, model, message}`
    (тот же тип, что в 1D). Ключ не возвращается никогда; `endpoint_host` — только хост.
  - `vector` = `{state, profile|null, message}`. `state`:
    `ready` — расширение `pgvector` установлено и embedding-провайдер настроен;
    `no_embeddings` — расширение есть, провайдера нет, векторы не считаются;
    `extension_missing` — `CREATE EXTENSION vector` не выполнен (миграция не может его
    выполнить: расширение не `trusted`, а роль миграций не суперпользователь).
  - `search_mode`: `hybrid` | `keyword_only`. Это фактический режим, а не намерение.
  - `limits` = `{max_query_chars, max_results, max_answer_claims, max_answer_chars,
    chunk_max_chars}`. Размерности вектора здесь нет намеренно: она неизвестна, пока не
    сделан хотя бы один вызов, и поле, всегда равное `null`, — это обещание, которого
    система не выполняет.

**Версии знаний.** Все ответы ограничены партнёром внутри бюро.

- `GET /api/partners/{id}/versions` → `{items: [KnowledgeVersion]}`, новые первыми.
- `GET /api/partners/{id}/versions/{version_id}` → `KnowledgeVersion`.
- `GET /api/partners/{id}/versions/{version_id}/claims` → `{items: [VersionClaim]}`.
- `GET /api/partners/{id}/versions/{version_id}/gaps` → `{items: [VersionGap]}`.
- `GET /api/partners/{id}/validation` → `{provider, published: KnowledgeVersion|null,
  runs: [ValidationRun], versions: [KnowledgeVersion], candidates: CandidateSummary}`.
  `candidates` = `{facts, findings, gaps_open, materials_drafted}` — сколько кандидатов
  сейчас есть у партнёра, чтобы интерфейс не предлагал проверку там, где проверять нечего.
- `POST /api/partners/{id}/validate` → `ValidationRun`. Ставит в очередь проверку всех
  кандидатов партнёра. Идемпотентен: пока проверка `queued`/`running`, повтор возвращает тот
  же запуск. 409 `conflict` — у партнёра нет ни одного кандидата.
- `POST /api/partners/{id}/versions/{version_id}/retract` `{reason}` → `KnowledgeVersion`.
  Переводит версию в `revoked`. 409 `conflict`, если версия не `published`. `reason` — 1–1000
  символов, обязателен: отзыв без причины не отличить от сбоя.

`KnowledgeVersion`: `{id, partner_id, number, status, validation_run_id, input_fingerprint,
claims_total, claims_source_supported, claims_hypothesis, claims_unknown, claims_conflicted,
claims_stale, gaps_open, chunks_total, chunks_embedded, embedding_profile, readiness:
[ReadinessEntry], blocked_reasons[], created_at, published_at, superseded_at, revoked_at,
revoked_reason}`.

- `status`: `draft` | `validating` | `published` | `blocked` | `superseded` | `revoked`
  (`block-01-spec.md` §7). `blocked` — правила готовности не выполнены: версия существует как
  запись проверки, но **не публикуется**, и `blocked_reasons` говорит словами, почему.
- `number` — порядковый номер версии партнёра, начиная с 1. Он растёт и не переиспользуется.
- У партнёра в каждый момент не больше одной версии со статусом `published` — это
  гарантировано частичным уникальным индексом, а не порядком операций в коде.
- `input_fingerprint` — отпечаток входа (какие кандидаты и какие их ревизии вошли).
  Запоздавший запуск, чей отпечаток старше уже опубликованного, не публикуется
  (`block-01-spec.md` §7).
- Счётчики вычисляются из строк снимка при чтении.

`ReadinessEntry`: `{topic, state, reason}`.

- `topic`: `product_description` | `audience_hypotheses` | `characteristic_answers` |
  `commercial_answers` (§7: готовность определяется отдельно для описания продукта, гипотез
  аудитории, ответов о характеристиках и ответов о коммерческих условиях).
- `state`: `ready` | `limited` | `blocked`. `limited` — отвечать можно, но с оговорками, и
  `reason` называет их словами.
- Готовность — это **доступность знаний**, а не разрешение на рассылку, сделку или обещание
  совместимости. Интерфейс обязан говорить это рядом.

`VersionClaim`: `{id, version_id, origin, origin_id, scope, product_name, kind, status,
attribute, value_text, unit, conditions, model_context, check_note, evidence:
[VersionEvidence], created_at}`.

- `origin`: `partner_material` (факт 1C) | `industry_research` (вывод 1D). `origin_id` —
  идентификатор исходного кандидата; он существует для прослеживаемости и **не** делает
  снимок зависимым от кандидата: удаление кандидата не меняет опубликованную версию.
- `scope`: `partner` | `industry`. Отраслевое утверждение остаётся отраслевым и внутри
  версии: у него нет и не может быть `product_name` (`block-01-plan.md`, 1D §4).
- `status`: `source_supported` | `hypothesis` | `unknown` | `conflicted` | `stale`
  (`block-01-spec.md` §6.7). `source_supported` означает **поддержку источником**, а не
  независимую проверку производителем и не гарантию истинности.
- `check_note` — словами, почему статус такой. Интерфейс показывает его как есть.
- `evidence` никогда не пуст (отложенный триггер БД), и цитаты **скопированы в версию**:
  снимок не ссылается на текст страницы, который может быть перечитан.

`VersionEvidence`: `{id, claim_id, source_kind, material_id, material_filename, page_number,
region_id, url, host, retrieved_at, content_hash, quote, char_start, char_end}`.

- `source_kind`: `material` | `external`. Поля другого вида — `null`. Оригинал партнёра
  открывается существующим `.../original#page=N`; внешний источник — своим `url`.
- `quote` — дословный фрагмент, скопированный в версию в момент публикации.

`VersionGap`: `{id, version_id, origin_id, product_name, topic, missing, blocks,
blocks_topics[], created_at}`. `blocks_topics` — какие из четырёх готовностей этот пробел
ограничивает; пробел показывает, какие ответы он блокирует (`block-01-spec.md` §11).

`ValidationRun`: `{id, partner_id, status, prompt_profile, version_id, version_number,
claims_considered, claims_source_supported, claims_hypothesis, claims_unknown,
claims_conflicted, claims_stale, claims_rejected, gaps_carried, chunks_created,
chunks_embedded, model_reviewed, published, rejections[], blocked_reasons[], diagnostic,
started_at, finished_at, created_at}`.

- `status`: `queued` | `running` | `completed` | `partial` | `failed`. Значения
  `needs_provider` здесь **нет**: проверка детерминирована и не зависит от модели.
- `model_reviewed` — сколько утверждений получили второе мнение модели. `0` — нормальное
  состояние без ключа, и оно не понижает статус запуска.
- `published` — была ли версия опубликована этим запуском. `false` при
  `blocked_reasons` непустом.
- Один партнёр — одна строка текущего запуска; история попыток остаётся на задании.

`Job.kind` дополняется значением `validate_partner` (без `page_number` и без
`research_plan_id`). **`Job.material_id` становится nullable** — это единственное
не-аддитивное изменение существующего типа в этом этапе: проверка относится к партнёру, а
не к документу, и записать туда произвольный `material_id` значило бы записать неправду.
Для всех остальных видов задания поле присутствует, и это связано CHECK-ом в БД.

**Поиск и ответы.** Оба эндпоинта — `POST`, потому что запрос содержит текст, а не
идентификатор: текст вопроса не место в URL, в логах и в истории браузера.

- `POST /api/partners/{id}/retrieval/search` `{query, version_id?, product?, limit?}` →
  `SearchResponse`. `product` — название изделия; сравнение идёт по свёрнутой форме, так
  что регистр и пробелы не важны. Тело проверяется строго (`deny_unknown_fields`):
  неизвестное поле — 400, а не молча проигнорированный фильтр.
- `POST /api/partners/{id}/retrieval/answer` `{question, version_id?}` → `AnswerResponse`.

Область обязательна и берётся из пути и сессии, а не из тела (`block-01-spec.md` §9).
`version_id` закрепляет версию; он допускается только для версии этого партнёра со статусом
`published` или `superseded` — закреплённая когда-то версия остаётся читаемой, потому что в
этом и состоит смысл закрепления. Версия `revoked` отвергается 409 `conflict`; `draft`,
`validating` и `blocked` — 404, как несуществующие для поиска. Без `version_id` берётся
текущая опубликованная.

`SearchResponse`: `{state, version, mode, degraded[], items: [SearchHit], gaps: [VersionGap],
message}`.

- `state`: `ok` | `no_published_version` | `insufficient_evidence`.
  `no_published_version` — у партнёра нечего искать: проверка не проходила, была заблокирована
  или версия отозвана. Это не ошибка и не пустой результат, это названное состояние.
- `version` — `VersionRef` = `{id, number, status, published_at}` либо `null`.
- `mode`: `hybrid` | `keyword`. `degraded` — список причин словами, почему режим не полный
  (нет embedding-провайдера, нет расширения, у версии нет векторов её профиля).
- Результаты одного ответа всегда принадлежат **одной** версии (§9: «в один ответ не
  смешиваются разные версии»).

`SearchHit`: `{claim, score, matched_by[]}`. `matched_by` — непустое подмножество
`exact` | `keyword` | `vector`; `score` — внутренняя сопоставимость в пределах одного ответа,
а не вероятность и не процент уверенности.

`AnswerResponse`: `{state, version, mode, degraded[], text, answer_is_model_context,
claims: [VersionClaim], citations: [VersionEvidence], conditions[], gaps: [VersionGap],
readiness: [ReadinessEntry], limitations[], rejections[], message}`.

- `state`: `answered` | `evidence_only` | `insufficient_evidence` | `no_published_version`.
- **`answered` обязывает к цитатам.** При `state = answered` `citations` непуст, и каждая
  цитата принадлежит утверждению этой закреплённой версии этого партнёра. Ответ, чьи
  цитаты не разрешились, не отдаётся как ответ: он понижается до `evidence_only` с
  причиной в `rejections`.
- `evidence_only` — найденные утверждения с цитатами есть, прозаического ответа нет: модель
  не настроена либо её ответ не прошёл проверку. `text = null`.
- `insufficient_evidence` — версия есть, подходящих утверждений нет. `text = null`,
  `claims` и `citations` пусты, `gaps` называет пробел, если он записан. Догадка не
  подставляется (`block-01-spec.md` §13.5).
- `answer_is_model_context` — `true` всегда, когда `text` не `null`: прозаический ответ
  является формулировкой модели, а не цитатой, и интерфейс обязан пометить это.
- `limitations` — оговорки словами: ограниченная готовность, `conflicted` среди найденного,
  `stale` источник. Частичная готовность не выглядит как полный коммерческий допуск
  (§13.7).

## Следующие контракты

1F — обновления, расширение входов и приёмка. Контракт фиксируется до соответствующей
UI-интеграции. Не создавать работающие на вид заглушки этих разделов раньше времени.

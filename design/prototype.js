/*
  OTDEL — Блок 1. Дизайн-прототип (BASIS).
  Чисто клиентский демонстрационный код: нет сетевых запросов, нет реальной
  загрузки файлов, нет искусственных таймеров процента/времени обработки.

  Данные партнёра BASIS отражают реальный пилот (docs/block-01-spec.md):
  два файла на 32 и 12 страниц (44 страницы суммарно), безопасный факт —
  профиль BP21, сечение 41×21 мм (презентация, стр. 5). ИНН не указывается,
  чтобы не изобретать реквизит. «Тестовый партнёр» — заведомо синтетические
  данные для показа интерфейса, явно помечен как демонстрационный.

  Правило безопасности: любые имена файлов и введённый пользователем текст
  (название нового партнёра) вставляются только через textContent —
  никогда через innerHTML или конкатенацию в разметку.
*/
(function () {
  "use strict";

  var OTTO = {
    welcome: "assets/otto/welcome.png",
    mail: "assets/otto/mail.png",
    search: "assets/otto/search.png",
    question: "assets/otto/question.png",
    pause: "assets/otto/pause.png",
    success: "assets/otto/success.png",
    error: "assets/otto/error.png"
  };

  var UPLOAD_HINT = "Добавьте каталоги, презентации и фотографии.";

  /* ---------- Partners (mutable: new drafts are appended in memory only) ---------- */

  var PARTNERS = [
    {
      id: "basis",
      name: "BASIS",
      subtitle: "ООО «Уралкреп»",
      synthetic: false,
      defaultScenario: "partial"
    },
    {
      id: "test-partner",
      name: "Тестовый партнёр",
      subtitle: "демонстрационные данные",
      synthetic: true,
      defaultScenario: "empty"
    }
  ];

  var STATUS_TEXT = {
    empty: { badge: "Черновик", badgeClass: "", otto: "welcome",
      text: "Материалы не получены. Загрузите документы, чтобы начать разбор." },
    processing: { badge: "В обработке", badgeClass: "", otto: "search",
      text: "Идёт разбор материалов. Устойчивый темп, ошибок пока нет." },
    partial: { badge: "Частично готово", badgeClass: "badge--warn", otto: "pause",
      text: "Часть направлений собрана. Отдельные ответы ограничены пробелами." },
    ready: { badge: "Опубликовано", badgeClass: "", otto: "success",
      text: "Версия опубликована автоматически. Доступны ответы с цитатами." },
    error: { badge: "Требует внимания", badgeClass: "badge--error", otto: "error",
      text: "Один файл не читается. Остальные материалы это не затрагивает." }
  };

  var STATE_LABEL = {
    queued: "В очереди", running: "Обрабатывается", completed: "Готово",
    partial: "Частично", failed: "Ошибка"
  };

  /* ---------- Dataset: BASIS (real pilot, safe demo facts only) ---------- */

  var BASIS_DATA = {
    materials: {
      empty: [],
      processing: [
        { name: "Техническая документация BASIS 2025.pdf", pages: 32, state: "running",
          outcome: "Извлечение структуры: исход определён для 21 из 32 страниц." },
        { name: "Презентация BASIS 2025.pdf", pages: 12, state: "queued",
          outcome: "В очереди на разбор." }
      ],
      partial: [
        { name: "Техническая документация BASIS 2025.pdf", pages: 32, state: "completed",
          outcome: "32 страницы обработаны, в том числе 3 после распознавания." },
        { name: "Презентация BASIS 2025.pdf", pages: 12, state: "partial",
          outcome: "Завершено с ограничением: 1 страница отмечена как неясная — часть сведений не извлечена." }
      ],
      ready: [
        { name: "Техническая документация BASIS 2025.pdf", pages: 32, state: "completed",
          outcome: "Разобрано полностью: 32 из 32 страниц." },
        { name: "Презентация BASIS 2025.pdf", pages: 12, state: "completed",
          outcome: "Разобрано полностью: 12 из 12 страниц." }
      ],
      error: [
        { name: "Презентация BASIS 2025.pdf", pages: 12, state: "failed",
          outcome: "Не удалось прочитать файл целиком: возможно повреждён исходный экспорт. Загрузите файл заново." },
        { name: "Техническая документация BASIS 2025.pdf", pages: 32, state: "completed",
          outcome: "Разобрано: 32 из 32 страниц." }
      ]
    },
    knowledge: {
      empty: null,
      processing: null,
      partial: {
        ready: [
          { name: "Профиль BP21", note: "Сечение 41×21 мм — подтверждено источником.", evidenceId: "bp21" }
        ],
        missing: [
          { name: "Крепёж для профиля BP21", note: "Есть в материалах, но цена и сроки поставки не указаны.", evidenceId: null }
        ],
        qa: {
          question: "Какое сечение у профиля BP21?",
          answerPre: "По презентации сечение профиля составляет 41×21 мм ",
          answerPost: ".",
          evidenceId: "bp21"
        }
      },
      ready: {
        ready: [
          { name: "Профиль BP21", note: "Сечение 41×21 мм — подтверждено источником.", evidenceId: "bp21" }
        ],
        missing: [],
        qa: {
          question: "Какое сечение у профиля BP21?",
          answerPre: "По презентации сечение профиля составляет 41×21 мм ",
          answerPost: ".",
          evidenceId: "bp21"
        }
      },
      error: {
        ready: [
          { name: "Профиль BP21", note: "Сечение 41×21 мм — подтверждено источником.", evidenceId: "bp21" }
        ],
        missing: [
          { name: "Изделие из презентации BASIS 2025", note: "Файл не читается — характеристики пока не извлечены.", evidenceId: null }
        ],
        qa: {
          question: "Какое сечение у профиля BP21?",
          answerPre: "По презентации сечение профиля составляет 41×21 мм ",
          answerPost: ".",
          evidenceId: "bp21"
        }
      }
    },
    gaps: {
      empty: [],
      processing: [],
      partial: [
        { question: "Какая цена и срок поставки крепежа для профиля BP21?",
          blocks: "Блокирует: коммерческое предложение по этому крепежу.",
          note: "Уточнить может только производитель.", ownerOnly: true },
        { question: "Подходит ли профиль BP21 для наружного применения?",
          blocks: "Блокирует: ответ о применении вне помещений.",
          note: "Материал не найден ни у партнёра, ни в открытых источниках отрасли.", ownerOnly: false }
      ],
      ready: [
        { question: "Действует ли сертификат соответствия после 2027 года?",
          blocks: "Ограничивает: ответы о сроке действия сертификации.",
          note: "Требует подтверждения производителем при продлении.", ownerOnly: true }
      ],
      error: [
        { question: "Что известно об изделиях, упомянутых только в презентации?",
          blocks: "Блокирует: описание этих изделий, пока файл не прочитан.",
          note: "Появится после повторной загрузки читаемого файла.", ownerOnly: false }
      ]
    },
    versions: {
      empty: [],
      processing: [
        { title: "Версия 1", status: "draft", statusLabel: "Черновик", date: "готовится",
          meta: "Формируется по мере разбора материалов." }
      ],
      partial: [
        { title: "Версия 2", status: "published", statusLabel: "Опубликована", date: "10 сентября 2026",
          meta: "Автоматически, по правилам проверки. Часть применений ограничена пробелами." },
        { title: "Версия 1", status: "superseded", statusLabel: "Заменена", date: "2 сентября 2026",
          meta: "Заменена версией 2." }
      ],
      ready: [
        { title: "Версия 3", status: "published", statusLabel: "Опубликована", date: "12 сентября 2026",
          meta: "Автоматически, по правилам проверки. Полный набор направлений." },
        { title: "Версия 2", status: "superseded", statusLabel: "Заменена", date: "10 сентября 2026",
          meta: "Заменена версией 3." }
      ],
      error: [
        { title: "Версия 2", status: "published", statusLabel: "Опубликована", date: "10 сентября 2026",
          meta: "Действующая версия не затронута сбоем нового файла." },
        { title: "Версия 3 (черновик)", status: "blocked", statusLabel: "Заблокирована", date: "готовится",
          meta: "Публикация ждёт исправимого файла — недостоверные данные не публикуются." }
      ]
    },
    evidence: {
      bp21: {
        source: "Документ: Презентация BASIS 2025.pdf · страница 5, область «Профили»",
        quote: "«Профиль BP21: сечение 41×21 мм.»",
        context: "Модель уточнила, что речь идёт о номинальном сечении профиля, а не о фактическом допуске изготовления — это пояснение, а не часть цитаты.",
        status: "Статус: сведения подтверждены источником. Это не является независимой гарантией точности."
      }
    }
  };

  /* ---------- Dataset: synthetic (explicitly fake, for "Тестовый партнёр") ---------- */

  var SYNTHETIC_DATA = {
    materials: {
      empty: [],
      processing: [
        { name: "Демонстрационный файл 1.pdf", pages: 5, state: "running",
          outcome: "Разбор демонстрационного файла (синтетические данные)." }
      ],
      partial: [
        { name: "Демонстрационный файл 1.pdf", pages: 5, state: "completed",
          outcome: "Разобрано: 5 из 5 страниц (синтетические данные)." },
        { name: "Демонстрационный файл 2.pdf", pages: 3, state: "partial",
          outcome: "Частично: 1 страница отмечена как неясная (синтетические данные)." }
      ],
      ready: [
        { name: "Демонстрационный файл 1.pdf", pages: 5, state: "completed",
          outcome: "Разобрано полностью (синтетические данные)." },
        { name: "Демонстрационный файл 2.pdf", pages: 3, state: "completed",
          outcome: "Разобрано полностью (синтетические данные)." }
      ],
      error: [
        { name: "Демонстрационный файл 2.pdf", pages: 3, state: "failed",
          outcome: "Не удалось прочитать файл (синтетические данные)." },
        { name: "Демонстрационный файл 1.pdf", pages: 5, state: "completed",
          outcome: "Разобрано: 5 из 5 страниц." }
      ]
    },
    knowledge: {
      empty: null,
      processing: null,
      partial: {
        ready: [
          { name: "Демонстрационный товар А", note: "Синтетический пример готового описания.", evidenceId: "demo1" }
        ],
        missing: [
          { name: "Демонстрационный товар Б", note: "Синтетический пример: цена в демо-данных не задана.", evidenceId: null }
        ],
        qa: {
          question: "Демонстрационный вопрос про товар А?",
          answerPre: "Демонстрационный ответ ",
          answerPost: " (синтетические данные).",
          evidenceId: "demo1"
        }
      },
      ready: {
        ready: [
          { name: "Демонстрационный товар А", note: "Синтетический пример готового описания.", evidenceId: "demo1" }
        ],
        missing: [],
        qa: {
          question: "Демонстрационный вопрос про товар А?",
          answerPre: "Демонстрационный ответ ",
          answerPost: " (синтетические данные).",
          evidenceId: "demo1"
        }
      },
      error: {
        ready: [],
        missing: [
          { name: "Демонстрационный товар из повреждённого файла", note: "Синтетический пример: файл не прочитан.", evidenceId: null }
        ],
        qa: null
      }
    },
    gaps: {
      empty: [],
      processing: [],
      partial: [
        { question: "Демонстрационный пробел про товар Б?",
          blocks: "Синтетический пример ограничения ответа.",
          note: "Демонстрационные данные для показа интерфейса.", ownerOnly: false }
      ],
      ready: [],
      error: [
        { question: "Демонстрационный пробел про повреждённый файл?",
          blocks: "Синтетический пример ограничения ответа.",
          note: "Демонстрационные данные для показа интерфейса.", ownerOnly: false }
      ]
    },
    versions: {
      empty: [],
      processing: [
        { title: "Демо-версия 1", status: "draft", statusLabel: "Черновик", date: "готовится",
          meta: "Синтетический пример: формируется по мере разбора." }
      ],
      partial: [
        { title: "Демо-версия 2", status: "published", statusLabel: "Опубликована", date: "синтетическая дата",
          meta: "Синтетический пример автоматической публикации." }
      ],
      ready: [
        { title: "Демо-версия 2", status: "published", statusLabel: "Опубликована", date: "синтетическая дата",
          meta: "Синтетический пример автоматической публикации." }
      ],
      error: [
        { title: "Демо-версия 2", status: "published", statusLabel: "Опубликована", date: "синтетическая дата",
          meta: "Синтетический пример: действующая версия не затронута сбоем." }
      ]
    },
    evidence: {
      demo1: {
        source: "Демонстрационный файл 1.pdf · страница 2 (синтетические данные)",
        quote: "«Пример цитаты для демонстрации интерфейса.»",
        context: "Пример добавленного контекста модели — не является реальными данными.",
        status: "Статус: синтетический пример, не связан с реальным документом."
      }
    }
  };

  var DATASETS = { basis: BASIS_DATA, synthetic: SYNTHETIC_DATA };

  var state = {
    partnerId: "basis",
    scenario: "partial",
    activeTab: "materials",
    lastFocused: null
  };

  var els = {};
  var draftCounter = 0;

  function $(id) { return document.getElementById(id); }

  function cacheEls() {
    els.scenarioSelect = $("scenario-select");
    els.partnerList = $("partner-list");
    els.partnerName = $("partner-name");
    els.partnerSubtitle = $("partner-subtitle");
    els.partnerStatusLabel = $("partner-status-label");
    els.partnerStatusText = $("partner-status-text");
    els.partnerOtto = $("partner-otto");
    els.tabs = [$("tab-materials"), $("tab-knowledge"), $("tab-gaps"), $("tab-versions")];
    els.panels = {
      materials: $("panel-materials"),
      knowledge: $("panel-knowledge"),
      gaps: $("panel-gaps"),
      versions: $("panel-versions")
    };
    els.mobileMenuBtn = $("mobile-menu-btn");
    els.sidebar = $("sidebar");

    els.newPartnerDialog = $("new-partner-dialog");
    els.newPartnerForm = $("new-partner-form");
    els.openNewPartner = $("open-new-partner");
    els.cancelNewPartner = $("cancel-new-partner");
    els.partnerNameInput = $("partner-name-input");
    els.partnerNameError = $("partner-name-error");

    els.uploadDialog = $("upload-dialog");
    els.uploadPartnerLine = $("upload-partner-line");
    els.materialsFileInput = $("materials-file-input");
    els.materialsFileList = $("materials-file-list");
    els.closeUpload = $("close-upload");

    els.evidenceDrawer = $("evidence-drawer");
    els.evidenceBody = $("evidence-body");
    els.closeEvidence = $("close-evidence");

    els.toast = $("toast");
  }

  function currentPartner() {
    var i;
    for (i = 0; i < PARTNERS.length; i++) {
      if (PARTNERS[i].id === state.partnerId) return PARTNERS[i];
    }
    return PARTNERS[0];
  }

  function currentDataset() {
    return currentPartner().synthetic ? DATASETS.synthetic : DATASETS.basis;
  }

  /* ---------- Sidebar ---------- */

  function renderSidebar() {
    els.partnerList.textContent = "";
    PARTNERS.forEach(function (p) {
      var btn = document.createElement("button");
      btn.type = "button";
      btn.className = "partner-item";
      btn.setAttribute("aria-current", p.id === state.partnerId ? "true" : "false");
      var strong = document.createElement("strong");
      strong.textContent = p.name; // textContent only — name may come from user-created draft
      var span = document.createElement("span");
      span.textContent = p.subtitle;
      btn.appendChild(strong);
      btn.appendChild(span);
      btn.addEventListener("click", function () {
        state.partnerId = p.id;
        state.scenario = p.defaultScenario;
        els.scenarioSelect.value = p.defaultScenario;
        renderAll();
        if (window.matchMedia("(max-width: 767px)").matches) {
          closeMobileSidebar();
        }
      });
      els.partnerList.appendChild(btn);
    });
  }

  /* ---------- Header ---------- */

  function renderHeader() {
    var p = currentPartner();
    var s = STATUS_TEXT[state.scenario];
    els.partnerName.textContent = p.name; // textContent only
    els.partnerSubtitle.textContent = p.subtitle;
    els.partnerStatusLabel.textContent = s.badge;
    els.partnerStatusLabel.className = "status-label" + (s.badgeClass ? " " + s.badgeClass : "");
    els.partnerStatusText.textContent = s.text;
    els.partnerOtto.src = OTTO[s.otto];
  }

  /* ---------- Tabs ---------- */

  var TAB_ORDER = ["materials", "knowledge", "gaps", "versions"];

  function selectTab(tab, moveFocus) {
    state.activeTab = tab;
    els.tabs.forEach(function (btn) {
      var t = btn.id.replace("tab-", "");
      var selected = t === tab;
      btn.setAttribute("aria-selected", selected ? "true" : "false");
      btn.tabIndex = selected ? 0 : -1;
      if (selected && moveFocus) btn.focus();
    });
    TAB_ORDER.forEach(function (t) {
      els.panels[t].hidden = t !== tab;
    });
  }

  function initTabs() {
    els.tabs.forEach(function (btn, index) {
      btn.addEventListener("click", function () {
        selectTab(btn.id.replace("tab-", ""), false);
      });
      btn.addEventListener("keydown", function (e) {
        var newIndex = null;
        if (e.key === "ArrowRight") newIndex = (index + 1) % els.tabs.length;
        else if (e.key === "ArrowLeft") newIndex = (index - 1 + els.tabs.length) % els.tabs.length;
        else if (e.key === "Home") newIndex = 0;
        else if (e.key === "End") newIndex = els.tabs.length - 1;
        if (newIndex !== null) {
          e.preventDefault();
          selectTab(els.tabs[newIndex].id.replace("tab-", ""), true);
        }
      });
    });
  }

  /* ---------- Materials panel ---------- */

  function renderMaterials() {
    var panel = els.panels.materials;
    panel.textContent = "";
    var ds = currentDataset();
    var items = ds.materials[state.scenario] || [];

    var cta = document.createElement("div");
    cta.className = "upload-cta";
    var ctaText = document.createElement("p");
    ctaText.textContent = UPLOAD_HINT;
    var ctaBtn = document.createElement("button");
    ctaBtn.type = "button";
    ctaBtn.className = "button button-primary";
    ctaBtn.textContent = "Загрузить материалы";
    ctaBtn.addEventListener("click", function (e) { openUploadDialog(e.currentTarget); });
    cta.appendChild(ctaText);
    cta.appendChild(ctaBtn);
    panel.appendChild(cta);

    if (!items.length) {
      panel.appendChild(buildEmptyState(
        "welcome",
        "Материалы ещё не поступили",
        "Как только появятся файлы, здесь будет видно, какие страницы уже прочитаны, а какие требуют внимания."
      ));
      return;
    }

    var list = document.createElement("ul");
    list.className = "material-list";
    items.forEach(function (item) {
      var li = document.createElement("li");
      li.className = "material-item";
      li.dataset.state = item.state;

      var icon = document.createElement("div");
      icon.className = "material-icon";
      icon.textContent = "PDF";
      li.appendChild(icon);

      var body = document.createElement("div");
      body.className = "material-body";
      var strong = document.createElement("strong");
      strong.textContent = item.name;
      var meta = document.createElement("span");
      meta.className = "material-meta";
      meta.textContent = (item.pages ? item.pages + " стр. · " : "") + STATE_LABEL[item.state];
      var outcome = document.createElement("span");
      outcome.className = "material-outcome";
      outcome.textContent = item.outcome;
      body.appendChild(strong);
      body.appendChild(meta);
      body.appendChild(outcome);

      if (item.state === "failed") {
        var actions = document.createElement("div");
        actions.className = "material-actions";
        var retry = document.createElement("button");
        retry.type = "button";
        retry.className = "button button-outline";
        retry.textContent = "Повторить обработку";
        retry.addEventListener("click", function () {
          showToast("Демонстрация: повтор обработки не выполняется.");
        });
        actions.appendChild(retry);
        body.appendChild(actions);
      }

      li.appendChild(body);
      list.appendChild(li);
    });
    panel.appendChild(list);
  }

  /* ---------- Knowledge panel ---------- */

  function renderKnowledge() {
    var panel = els.panels.knowledge;
    panel.textContent = "";
    var ds = currentDataset();
    var data = ds.knowledge[state.scenario];

    if (!data) {
      panel.appendChild(buildEmptyState(
        state.scenario === "processing" ? "search" : "welcome",
        state.scenario === "processing" ? "Знания ещё собираются" : "Знаний пока нет",
        state.scenario === "processing"
          ? "Направления и характеристики собираются по мере чтения материалов, без ручной сортировки документов."
          : "Раздел заполнится после того, как появятся материалы для разбора."
      ));
      return;
    }

    var grid = document.createElement("div");
    grid.className = "knowledge-grid";

    grid.appendChild(buildKnowledgeGroup(
      ds,
      "Готово к описанию",
      "Характеристики подтверждены источником — можно использовать для описания продукта.",
      data.ready,
      false
    ));
    grid.appendChild(buildKnowledgeGroup(
      ds,
      "Требует уточнения (нет цены или условий)",
      "Характеристики есть, но коммерческая часть отсутствует — численный или ценовой ответ недоступен.",
      data.missing,
      true
    ));
    panel.appendChild(grid);

    if (data.qa) {
      var preview = document.createElement("section");
      preview.className = "answer-preview";
      var h2 = document.createElement("h2");
      h2.textContent = "Пример ответа с цитатами";
      var q = document.createElement("p");
      q.className = "answer-question";
      q.textContent = "Вопрос: " + data.qa.question;
      var a = document.createElement("p");
      a.className = "answer-text";
      a.appendChild(document.createTextNode(data.qa.answerPre));
      var sup = document.createElement("sup");
      var ev = ds.evidence[data.qa.evidenceId];
      var citeBtn = document.createElement("button");
      citeBtn.type = "button";
      citeBtn.className = "citation-btn";
      citeBtn.textContent = "[1]";
      if (ev) {
        citeBtn.addEventListener("click", function (e) { openEvidence(ev, e.currentTarget); });
      } else {
        citeBtn.disabled = true;
      }
      sup.appendChild(citeBtn);
      a.appendChild(sup);
      a.appendChild(document.createTextNode(data.qa.answerPost));
      preview.appendChild(h2);
      preview.appendChild(q);
      preview.appendChild(a);
      preview.appendChild(buildSourceButton(ds, data.qa.evidenceId, "button button-outline"));
      panel.appendChild(preview);
    }
  }

  function buildKnowledgeGroup(ds, title, desc, items, attention) {
    var group = document.createElement("section");
    group.className = "knowledge-group" + (attention ? " knowledge-group--attention" : "");
    var h2 = document.createElement("h2");
    h2.textContent = title;
    var p = document.createElement("p");
    p.textContent = desc;
    group.appendChild(h2);
    group.appendChild(p);

    if (!items.length) {
      var none = document.createElement("p");
      none.textContent = "Пока пусто.";
      group.appendChild(none);
      return group;
    }

    var ul = document.createElement("ul");
    ul.className = "fact-list";
    items.forEach(function (item) {
      var li = document.createElement("li");
      li.className = "fact-item";
      var div = document.createElement("div");
      var strong = document.createElement("strong");
      strong.textContent = item.name;
      var span = document.createElement("span");
      span.textContent = item.note;
      div.appendChild(strong);
      div.appendChild(span);
      li.appendChild(div);
      li.appendChild(buildSourceButton(ds, item.evidenceId, "text-button"));
      ul.appendChild(li);
    });
    group.appendChild(ul);
    return group;
  }

  /* Builds a "Показать источник" button, or an explicit disabled
     "Источник недоступен" state when no evidence entry exists for the fact. */
  function buildSourceButton(ds, evidenceId, className) {
    var ev = evidenceId ? ds.evidence[evidenceId] : null;
    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = className;
    if (ev) {
      btn.textContent = "Показать источник";
      btn.addEventListener("click", function (e) { openEvidence(ev, e.currentTarget); });
    } else {
      btn.textContent = "Источник недоступен";
      btn.disabled = true;
      btn.title = "Для этого пункта пока нет подтверждающей цитаты.";
    }
    return btn;
  }

  /* ---------- Gaps panel ---------- */

  function renderGaps() {
    var panel = els.panels.gaps;
    panel.textContent = "";
    var ds = currentDataset();
    var items = ds.gaps[state.scenario] || [];

    var heading = document.createElement("div");
    heading.className = "section-heading";
    var h2 = document.createElement("h2");
    h2.textContent = "Пробелы";
    var p = document.createElement("p");
    p.textContent = "Каждый пробел показывает, какие ответы он ограничивает, и можно ли закрыть его без производителя.";
    heading.appendChild(h2);
    heading.appendChild(p);
    panel.appendChild(heading);

    if (!items.length) {
      panel.appendChild(buildEmptyState(
        "question",
        "Пробелов пока нет",
        state.scenario === "empty"
          ? "Оценивать пробелы рано — материалы ещё не поступили."
          : "Пробелы появятся, если чего-то не хватит для ответа."
      ));
      return;
    }

    var ul = document.createElement("ul");
    ul.className = "gap-list";
    items.forEach(function (g) {
      var li = document.createElement("li");
      li.className = "gap-item";
      var q = document.createElement("p");
      q.className = "gap-question";
      q.textContent = g.question;
      var blocks = document.createElement("p");
      blocks.className = "gap-blocks";
      blocks.textContent = g.blocks;
      var note = document.createElement("p");
      note.className = "gap-note";
      note.textContent = g.note;
      var actions = document.createElement("div");
      actions.className = "gap-actions";
      var answerBtn = document.createElement("button");
      answerBtn.type = "button";
      answerBtn.className = "button button-outline";
      answerBtn.textContent = "Ответить";
      answerBtn.addEventListener("click", function () {
        showToast("Демонстрация: ответ на пробел не сохраняется.");
      });
      var addBtn = document.createElement("button");
      addBtn.type = "button";
      addBtn.className = "button button-ghost";
      addBtn.textContent = "Добавить материал";
      addBtn.addEventListener("click", function (e) { openUploadDialog(e.currentTarget); });
      actions.appendChild(answerBtn);
      actions.appendChild(addBtn);
      li.appendChild(q);
      li.appendChild(blocks);
      li.appendChild(note);
      li.appendChild(actions);
      ul.appendChild(li);
    });
    panel.appendChild(ul);
  }

  /* ---------- Versions panel ---------- */

  function renderVersions() {
    var panel = els.panels.versions;
    panel.textContent = "";
    var ds = currentDataset();
    var items = ds.versions[state.scenario] || [];

    var heading = document.createElement("div");
    heading.className = "section-heading";
    var h2 = document.createElement("h2");
    h2.textContent = "Версии";
    heading.appendChild(h2);
    panel.appendChild(heading);

    if (!items.length) {
      panel.appendChild(buildEmptyState(
        "welcome",
        "Версий ещё нет",
        "Первая версия появится автоматически, когда материалы пройдут проверку."
      ));
      return;
    }

    var ul = document.createElement("ul");
    ul.className = "version-list";
    items.forEach(function (v) {
      var li = document.createElement("li");
      li.className = "version-item";
      var head = document.createElement("div");
      head.className = "version-item__head";
      var strong = document.createElement("strong");
      strong.textContent = v.title;
      var badge = document.createElement("span");
      badge.className = "badge" + (v.status === "blocked" ? " badge--error" : "");
      badge.textContent = v.statusLabel;
      head.appendChild(strong);
      head.appendChild(badge);
      var meta = document.createElement("p");
      meta.className = "version-meta";
      meta.textContent = v.date + " · " + v.meta;
      var link = document.createElement("button");
      link.type = "button";
      link.className = "text-button";
      link.textContent = "Просмотреть изменения";
      link.addEventListener("click", function () {
        showToast("Демонстрация: журнал изменений не подключён к реальным данным.");
      });
      li.appendChild(head);
      li.appendChild(meta);
      li.appendChild(link);
      ul.appendChild(li);
    });
    panel.appendChild(ul);

    var note = document.createElement("p");
    note.className = "version-note";
    note.textContent = "Публикация выполняется автоматически при выполнении правил проверки. Ручной просмотр остаётся возможностью, а не обязательным шагом.";
    panel.appendChild(note);
  }

  /* ---------- Shared: empty state ---------- */

  function buildEmptyState(otto, title, text) {
    var wrap = document.createElement("div");
    wrap.className = "empty-state";
    var img = document.createElement("img");
    img.src = OTTO[otto];
    img.alt = "";
    img.setAttribute("aria-hidden", "true");
    var h2 = document.createElement("h2");
    h2.textContent = title;
    var p = document.createElement("p");
    p.textContent = text;
    wrap.appendChild(img);
    wrap.appendChild(h2);
    wrap.appendChild(p);
    return wrap;
  }

  /* ---------- Evidence drawer ---------- */

  function openEvidence(ev, opener) {
    els.evidenceBody.textContent = "";

    var source = document.createElement("p");
    source.className = "drawer__source";
    source.textContent = ev.source;

    var quote = document.createElement("blockquote");
    quote.className = "drawer__quote";
    quote.textContent = ev.quote;

    var context = document.createElement("div");
    context.className = "drawer__context";
    var contextLabel = document.createElement("strong");
    contextLabel.textContent = "Добавленный контекст модели (не цитата)";
    context.appendChild(contextLabel);
    context.appendChild(document.createTextNode(ev.context));

    var status = document.createElement("p");
    status.className = "drawer__status";
    status.textContent = ev.status;

    var originalBtn = document.createElement("button");
    originalBtn.type = "button";
    originalBtn.className = "button button-outline";
    originalBtn.textContent = "Открыть оригинал страницы";

    // Feedback for this action stays inside the open dialog (role="status"),
    // not as a page-level toast hidden behind the inert background.
    var inlineStatus = document.createElement("p");
    inlineStatus.className = "drawer__inline-status";
    inlineStatus.setAttribute("role", "status");
    inlineStatus.setAttribute("aria-live", "polite");

    originalBtn.addEventListener("click", function () {
      inlineStatus.textContent = "Демонстрация: оригинал страницы недоступен в макете.";
    });

    els.evidenceBody.appendChild(source);
    els.evidenceBody.appendChild(quote);
    els.evidenceBody.appendChild(context);
    els.evidenceBody.appendChild(status);
    els.evidenceBody.appendChild(originalBtn);
    els.evidenceBody.appendChild(inlineStatus);

    openDialog(els.evidenceDrawer, opener);
  }

  /* ---------- Dialog helpers (native <dialog>: showModal makes background inert,
     which contains focus; 'close'/'cancel' cover Escape). Every call site passes
     the actual button that triggered the open, so focus returns there. ---------- */

  function openDialog(dialog, opener) {
    state.lastFocused = opener || document.activeElement;
    if (typeof dialog.showModal === "function") {
      dialog.showModal();
    } else {
      dialog.setAttribute("open", "");
    }
  }

  function closeDialogAndRestoreFocus(dialog) {
    if (dialog.open) dialog.close();
  }

  function attachDialogFocusReturn(dialog) {
    dialog.addEventListener("close", function () {
      if (state.lastFocused && typeof state.lastFocused.focus === "function") {
        state.lastFocused.focus();
      }
    });
  }

  function attachBackdropClose(dialog) {
    dialog.addEventListener("mousedown", function (e) {
      var rect = dialog.getBoundingClientRect();
      var inside = rect.top <= e.clientY && e.clientY <= rect.top + rect.height &&
        rect.left <= e.clientX && e.clientX <= rect.left + rect.width;
      if (!inside) closeDialogAndRestoreFocus(dialog);
    });
  }

  /* ---------- New partner dialog: creates an in-memory draft only ---------- */

  function openNewPartnerDialog(opener) {
    els.newPartnerForm.reset();
    clearNameError();
    openDialog(els.newPartnerDialog, opener || els.openNewPartner);
  }

  function clearNameError() {
    els.partnerNameError.hidden = true;
    els.partnerNameError.textContent = "";
    els.partnerNameInput.removeAttribute("aria-invalid");
  }

  function showNameError(message) {
    els.partnerNameError.hidden = false;
    els.partnerNameError.textContent = message;
    els.partnerNameInput.setAttribute("aria-invalid", "true");
    els.partnerNameInput.focus();
  }

  function createDraftPartner(name) {
    draftCounter += 1;
    var id = "draft-" + draftCounter;
    var partner = {
      id: id,
      name: name, // user text, always rendered via textContent
      subtitle: "Черновик · только в этой вкладке, не сохраняется",
      synthetic: true,
      defaultScenario: "empty"
    };
    PARTNERS.push(partner);
    state.partnerId = id;
    state.scenario = "empty";
    els.scenarioSelect.value = "empty";
    renderAll();
  }

  /* ---------- Upload dialog: materials for the CURRENT partner, name not required ---------- */

  function openUploadDialog(opener) {
    els.uploadPartnerLine.textContent = "Материалы для: " + currentPartner().name;
    els.materialsFileList.textContent = "";
    els.materialsFileInput.value = "";
    openDialog(els.uploadDialog, opener);
  }

  function renderFileList(listEl, fileList) {
    listEl.textContent = "";
    if (!fileList || !fileList.length) return;
    Array.prototype.forEach.call(fileList, function (file) {
      var li = document.createElement("li");
      var name = document.createElement("span");
      name.textContent = file.name; // textContent only — never innerHTML with file names
      var size = document.createElement("span");
      size.textContent = formatSize(file.size);
      li.appendChild(name);
      li.appendChild(size);
      listEl.appendChild(li);
    });
  }

  function formatSize(bytes) {
    if (!bytes && bytes !== 0) return "";
    if (bytes < 1024) return bytes + " Б";
    if (bytes < 1024 * 1024) return Math.round(bytes / 1024) + " КБ";
    return (bytes / (1024 * 1024)).toFixed(1) + " МБ";
  }

  /* ---------- Toast (only used when no modal dialog is open) ---------- */

  var toastTimer = null;

  function showToast(message) {
    els.toast.textContent = message;
    els.toast.hidden = false;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(function () {
      els.toast.hidden = true;
    }, 4000);
  }

  /* ---------- Mobile sidebar ---------- */

  function closeMobileSidebar() {
    els.sidebar.classList.remove("is-open");
    els.mobileMenuBtn.setAttribute("aria-expanded", "false");
  }

  function toggleMobileSidebar() {
    var open = els.sidebar.classList.toggle("is-open");
    els.mobileMenuBtn.setAttribute("aria-expanded", open ? "true" : "false");
  }

  /* ---------- Render all ---------- */

  function renderAll() {
    renderSidebar();
    renderHeader();
    renderMaterials();
    renderKnowledge();
    renderGaps();
    renderVersions();
  }

  function init() {
    cacheEls();
    els.scenarioSelect.value = state.scenario;

    initTabs();
    renderAll();

    els.scenarioSelect.addEventListener("change", function () {
      state.scenario = els.scenarioSelect.value;
      renderAll();
    });

    els.mobileMenuBtn.addEventListener("click", toggleMobileSidebar);

    els.openNewPartner.addEventListener("click", function (e) {
      openNewPartnerDialog(e.currentTarget);
    });
    els.cancelNewPartner.addEventListener("click", function () {
      closeDialogAndRestoreFocus(els.newPartnerDialog);
    });
    attachDialogFocusReturn(els.newPartnerDialog);
    attachBackdropClose(els.newPartnerDialog);

    els.partnerNameInput.addEventListener("input", clearNameError);

    els.newPartnerForm.addEventListener("submit", function (e) {
      e.preventDefault();
      var name = els.partnerNameInput.value.trim();
      if (!name) {
        showNameError("Укажите название или рабочее обозначение — поле обязательно.");
        return;
      }
      createDraftPartner(name);
      closeDialogAndRestoreFocus(els.newPartnerDialog);
      showToast("Черновик «" + name + "» добавлен только в этой вкладке — данные не сохраняются.");
    });

    els.closeUpload.addEventListener("click", function () {
      closeDialogAndRestoreFocus(els.uploadDialog);
    });
    attachDialogFocusReturn(els.uploadDialog);
    attachBackdropClose(els.uploadDialog);
    els.materialsFileInput.addEventListener("change", function () {
      renderFileList(els.materialsFileList, els.materialsFileInput.files);
    });

    els.closeEvidence.addEventListener("click", function () {
      closeDialogAndRestoreFocus(els.evidenceDrawer);
    });
    attachDialogFocusReturn(els.evidenceDrawer);
    attachBackdropClose(els.evidenceDrawer);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();

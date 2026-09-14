//! Phase 1F — what is out of date, what starts a new cycle, and reading a document again.
//!
//! Two rules hold for everything in this module.
//!
//! **Nothing here invents progress.** [`status`] reports states derived from rows;
//! [`refresh`] reports, per stage, what it actually queued and why it did not queue the
//! rest. A stage that cannot run because its adapter is unconfigured is named as such,
//! never skipped silently — a pipeline that stops at 1C looks exactly like one that
//! finished if nobody says which happened.
//!
//! **Nothing here changes published knowledge.** Both mutating endpoints only enqueue
//! work; the checker in the worker still decides what is published, and the database
//! still refuses to alter a snapshot.
//!
//! The history and the retention policy live in [`crate::routes::history`]; comparing two
//! versions and exporting one live in [`crate::routes::publication`], beside the other
//! endpoints addressed by version id.

use axum::extract::State;
use axum::Json;
use chrono::Utc;
use otdel_core::model::MaterialStatus;
use otdel_core::publication::{KnowledgeVersion, VersionStatus};
use otdel_core::updates::{
    EventActor, EventKind, RefreshPlan, RefreshReason, RefreshReasonCode, RefreshState,
    RefreshStatus, RefreshStep, RefreshStepKind, RefreshStepOutcome, SourceRefresh, SourceState,
    VersionRef,
};
use otdel_db::{events, jobs, materials, partners, publication, publication_read, updates};
use tracing::info;
use uuid::Uuid;

use crate::auth::Session;
use crate::error::{ApiError, ApiResult};
use crate::extract::ApiPath;
use crate::routes::partners::partner_not_found;
use crate::state::AppState;

// --- refresh status ---------------------------------------------------------------------

/// `GET /api/partners/{id}/refresh`
///
/// Where the published knowledge stands relative to the partner's documents, and why.
///
/// Everything in the answer is computed from rows that exist. There is no stored "stale"
/// flag, because a stored flag is a claim some code has to remember to update and is
/// silently wrong the first time somebody forgets.
pub async fn status(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<RefreshStatus>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }

    let published = publication_read::find_published(&mut tx, partner_id).await?;
    let versions = publication_read::list_versions(&mut tx, partner_id).await?;
    let sources = updates::source_states(&mut tx, partner_id).await?;
    // The digest loader, not the checker's: this endpoint is polled while a check runs,
    // and the fingerprint is computed over values and quotations rather than over the
    // documents they came from.
    let candidates = updates::load_candidate_digest(&mut tx, partner_id).await?;
    let published_candidate =
        publication_read::published_candidate_fingerprint(&mut tx, partner_id)
            .await?
            .flatten();
    let checking = jobs::validation_pending(&mut tx, partner_id).await?;
    let run = publication_read::find_run(&mut tx, partner_id).await?;
    tx.commit().await?;

    let fingerprint_now = otdel_publish::candidate_fingerprint(&candidates);
    let latest = versions.first().map(version_ref);

    let mut reasons: Vec<RefreshReason> = Vec::new();
    let source_views: Vec<SourceRefresh> = sources
        .iter()
        .map(|source| {
            let state = source_state(source);
            if let Some(reason) = source_reason(source, state) {
                reasons.push(reason);
            }
            SourceRefresh {
                material_id: source.material_id,
                filename: source.filename.clone(),
                material_status: source.status,
                state,
                content_revision: source.content_revision,
                drafted_revision: source.drafted_revision,
                draft_status: source.run_status.clone(),
                facts_drafted: i32::try_from(source.facts_drafted).unwrap_or(i32::MAX),
                claims_in_published: i32::try_from(source.claims_in_published).unwrap_or(i32::MAX),
                message: source_message(source, state),
            }
        })
        .collect();

    // The candidate comparison. Three outcomes, and the middle one is the reason this
    // exists at all: a published version that predates the fingerprint cannot be compared,
    // and saying "unchanged" about it would be an answer nobody computed.
    match (&published, &published_candidate) {
        (Some(version), Some(stored)) if stored != &fingerprint_now => {
            reasons.push(RefreshReason {
                code: RefreshReasonCode::CandidatesChanged,
                message: "набор кандидатов отличается от того, из которого построена \
                          опубликованная версия: новая проверка даст другую версию"
                    .to_owned(),
                material_id: None,
                material_filename: None,
                version_id: Some(version.id),
                version_number: Some(version.number),
                content_revision: None,
                drafted_revision: None,
            });
        }
        (Some(version), None) => {
            reasons.push(RefreshReason {
                code: RefreshReasonCode::ComparisonUnavailable,
                message: "опубликованная версия сделана до того, как появился отпечаток \
                          кандидатов: сравнить без запуска проверки нельзя"
                    .to_owned(),
                material_id: None,
                material_filename: None,
                version_id: Some(version.id),
                version_number: Some(version.number),
                content_revision: None,
                drafted_revision: None,
            });
        }
        _ => {}
    }

    // The last check, when it did not publish. A blocked or failed check is the most
    // common reason for "я загрузил материал, а версия прежняя".
    if let Some(run) = &run {
        if let Some(first) = run.blocked_reasons.first() {
            reasons.push(RefreshReason {
                code: RefreshReasonCode::LastCheckBlocked,
                message: format!("последняя проверка не опубликовала версию: {first}"),
                material_id: None,
                material_filename: None,
                version_id: run.version_id,
                version_number: run.version_number,
                content_revision: None,
                drafted_revision: None,
            });
        }
        if run.status == otdel_core::publication::ValidationRunStatus::Failed {
            reasons.push(RefreshReason {
                code: RefreshReasonCode::LastCheckFailed,
                message: run
                    .diagnostic
                    .clone()
                    .unwrap_or_else(|| "последняя проверка завершилась ошибкой".to_owned()),
                material_id: None,
                material_filename: None,
                version_id: run.version_id,
                version_number: run.version_number,
                content_revision: None,
                drafted_revision: None,
            });
        }
    }

    // A version that was withdrawn and not replaced. Reported before "nothing published",
    // because the two look the same to search and are entirely different to a person.
    let retracted = versions
        .iter()
        .find(|version| version.status == VersionStatus::Revoked && published.is_none());
    if let Some(version) = retracted {
        reasons.push(RefreshReason {
            code: RefreshReasonCode::VersionRetracted,
            message: format!(
                "версия {} отозвана: {}. Ответов по опубликованной версии сейчас нет",
                version.number,
                version
                    .revoked_reason
                    .as_deref()
                    .unwrap_or("причина не записана")
            ),
            material_id: None,
            material_filename: None,
            version_id: Some(version.id),
            version_number: Some(version.number),
            content_revision: None,
            drafted_revision: None,
        });
    } else if published.is_none() {
        reasons.push(RefreshReason {
            code: RefreshReasonCode::NothingPublished,
            message: "у партнёра нет опубликованной версии: искать и отвечать пока не по \
                      чему"
                .to_owned(),
            material_id: None,
            material_filename: None,
            version_id: None,
            version_number: None,
            content_revision: None,
            drafted_revision: None,
        });
    }

    let refresh_state = if checking {
        RefreshState::Checking
    } else if published.is_none() {
        if retracted.is_some() {
            RefreshState::Retracted
        } else {
            RefreshState::NeverPublished
        }
    } else if reasons.is_empty() {
        RefreshState::Current
    } else {
        RefreshState::RevalidationRequired
    };

    let message = describe(refresh_state, &reasons);

    Ok(Json(RefreshStatus {
        state: refresh_state,
        published: published.as_ref().map(version_ref),
        latest,
        reasons,
        sources: source_views,
        candidate_fingerprint: fingerprint_now,
        published_candidate_fingerprint: published_candidate,
        checking,
        message,
        computed_at: Utc::now(),
    }))
}

/// `POST /api/partners/{id}/refresh`
///
/// Start whatever the refresh status says is missing, in dependency order, and report
/// what actually happened to each stage.
///
/// This is an operational control, not a promise: it enqueues, and the queue and the
/// worker do the rest. Every step comes back with one of five outcomes and only
/// [`RefreshStepOutcome::Queued`] means work will happen.
pub async fn refresh(
    State(state): State<AppState>,
    session: Session,
    ApiPath(partner_id): ApiPath<Uuid>,
) -> ApiResult<Json<RefreshPlan>> {
    let provider_ready = state.llm.describe().state == "ready";

    let mut tx = state.db.begin_scoped(session.bureau_id).await?;
    if !partners::exists(&mut tx, partner_id).await? {
        return Err(partner_not_found());
    }

    let sources = updates::source_states(&mut tx, partner_id).await?;
    let mut steps: Vec<RefreshStep> = Vec::new();

    for source in &sources {
        let state_of = source_state(source);
        match state_of {
            // Being read already, or queued for it: joining is the whole point of an
            // idempotent queue.
            SourceState::Reading => steps.push(RefreshStep {
                kind: RefreshStepKind::Extraction,
                outcome: RefreshStepOutcome::AlreadyRunning,
                material_id: Some(source.material_id),
                material_filename: Some(source.filename.clone()),
                job_id: None,
                message: format!("«{}» уже читается или стоит в очереди", source.filename),
            }),
            SourceState::Unreadable => steps.push(RefreshStep {
                kind: RefreshStepKind::Extraction,
                outcome: RefreshStepOutcome::Waiting,
                material_id: Some(source.material_id),
                material_filename: Some(source.filename.clone()),
                job_id: None,
                message: format!(
                    "«{}»: пригодного текста не получено, разбирать нечего. Перечитать \
                     можно отдельной кнопкой",
                    source.filename
                ),
            }),
            SourceState::NotDrafted | SourceState::RereadAfterDraft => {
                // The adapter is checked **first**, and that order is the honest one.
                //
                // Extraction queues a draft by itself, so a readable document almost
                // always has an `understand_material` job waiting. With no model
                // configured that job will be claimed, record `needs_provider` and store
                // nothing — so answering "уже в очереди" would be literally true and
                // practically a lie: it invites the owner to wait for something that
                // cannot succeed. What they can act on is the missing key.
                if !provider_ready {
                    steps.push(RefreshStep {
                        kind: RefreshStepKind::Understanding,
                        outcome: RefreshStepOutcome::NeedsProvider,
                        material_id: Some(source.material_id),
                        material_filename: Some(source.filename.clone()),
                        job_id: None,
                        message: format!(
                            "«{}» ждёт разбора, но модель продуктолога не настроена: {}",
                            source.filename,
                            state.llm.describe().message
                        ),
                    });
                    continue;
                }
                if jobs::understanding_pending(&mut tx, source.material_id).await? {
                    steps.push(RefreshStep {
                        kind: RefreshStepKind::Understanding,
                        outcome: RefreshStepOutcome::AlreadyRunning,
                        material_id: Some(source.material_id),
                        material_filename: Some(source.filename.clone()),
                        job_id: None,
                        message: format!("разбор «{}» уже в очереди", source.filename),
                    });
                    continue;
                }
                let job =
                    jobs::enqueue_understanding(&mut tx, partner_id, source.material_id).await?;
                otdel_db::knowledge::enqueue_run(
                    &mut tx,
                    partner_id,
                    source.material_id,
                    otdel_knowledge::PROMPT_PROFILE,
                )
                .await?;
                steps.push(RefreshStep {
                    kind: RefreshStepKind::Understanding,
                    outcome: RefreshStepOutcome::Queued,
                    material_id: Some(source.material_id),
                    material_filename: Some(source.filename.clone()),
                    job_id: Some(job.id),
                    message: match state_of {
                        SourceState::RereadAfterDraft => format!(
                            "«{}» перечитан после разбора — разбор поставлен в очередь заново",
                            source.filename
                        ),
                        _ if source.run_status.is_some() => format!(
                            "«{}»: предыдущий разбор не дал кандидатов — поставлен заново",
                            source.filename
                        ),
                        _ => format!("«{}» поставлен в очередь на разбор", source.filename),
                    },
                });
            }
            SourceState::Drafted => steps.push(RefreshStep {
                kind: RefreshStepKind::Understanding,
                outcome: RefreshStepOutcome::UpToDate,
                material_id: Some(source.material_id),
                material_filename: Some(source.filename.clone()),
                job_id: None,
                message: format!(
                    "«{}» разобран по текущему чтению ({} фактов)",
                    source.filename, source.facts_drafted
                ),
            }),
        }
    }

    // The check, once. It is partner-wide: a contradiction between two documents is only
    // visible from there, so queueing one per material would be both wasteful and wrong.
    let candidates = publication_read::candidate_summary(&mut tx, partner_id).await?;
    if jobs::validation_pending(&mut tx, partner_id).await? {
        steps.push(RefreshStep {
            kind: RefreshStepKind::Validation,
            outcome: RefreshStepOutcome::AlreadyRunning,
            material_id: None,
            material_filename: None,
            job_id: None,
            message: "проверка уже идёт. Кандидаты, появившиеся после её начала, в эту \
                      версию не войдут — по ним автоматически запустится следующая проверка"
                .to_owned(),
        });
    } else if candidates.is_empty() {
        steps.push(RefreshStep {
            kind: RefreshStepKind::Validation,
            outcome: RefreshStepOutcome::Waiting,
            material_id: None,
            material_filename: None,
            job_id: None,
            message: "проверять нечего: кандидатов нет. Проверка запустится сама, когда \
                      разбор что-нибудь найдёт"
                .to_owned(),
        });
    } else {
        let run =
            publication::enqueue_run(&mut tx, partner_id, otdel_publish::PROMPT_PROFILE).await?;
        let job = jobs::enqueue_validation(&mut tx, partner_id, run.id).await?;
        steps.push(RefreshStep {
            kind: RefreshStepKind::Validation,
            outcome: RefreshStepOutcome::Queued,
            material_id: None,
            material_filename: None,
            job_id: Some(job.id),
            message: "проверка и публикация поставлены в очередь".to_owned(),
        });
    }

    let queued = i32::try_from(
        steps
            .iter()
            .filter(|step| step.outcome == RefreshStepOutcome::Queued)
            .count(),
    )
    .unwrap_or(i32::MAX);

    events::record(
        &mut tx,
        &events::NewEvent::new(
            EventKind::RefreshRequested,
            EventActor::Owner,
            format!(
                "владелец запросил обновление: поставлено в очередь шагов — {queued} из {}",
                steps.len()
            ),
        )
        .for_partner(partner_id)
        // Counts, not one entry per step. `steps` has one element per material, and the
        // column is bounded at 4 kB — a partner with enough documents would have overflowed
        // the CHECK, aborting the very transaction that had just queued the work, every
        // time and with no way to make progress. A fixed-size summary cannot do that, and
        // the per-step detail is in the response the owner is looking at.
        .with_detail(serde_json::json!({
            "queued": queued,
            "steps_total": steps.len(),
            "outcomes": outcome_counts(&steps),
        })),
    )
    .await?;
    tx.commit().await?;

    let message = if queued == 0 {
        "Ничего запускать не потребовалось: всё либо уже выполняется, либо не изменилось. \
         Причины по каждому шагу — ниже."
            .to_owned()
    } else {
        format!(
            "Поставлено в очередь шагов: {queued}. Ход работы виден в журнале и в \
                 состоянии материалов."
        )
    };

    info!(partner_id = %partner_id, queued, "refresh requested by the owner");

    Ok(Json(RefreshPlan {
        steps,
        queued,
        message,
        requested_at: Utc::now(),
    }))
}

// --- reprocessing one document -----------------------------------------------------------

/// `POST /api/partners/{id}/materials/{material_id}/reprocess`
///
/// Read a document again, on purpose.
///
/// The difference from `.../retry` is not cosmetic. Retry resumes reading that did not
/// finish and refuses a `completed` material; this accepts exactly the finished ones,
/// which is `block-01-spec.md` §6.1's «явный запуск новой версии обработчика». The stored
/// original is untouched — it is the same bytes and the same identifier — and so is every
/// published version that cites it, because a snapshot carries copies of its quotations.
///
/// What does change is `content_revision`, and that is the point: any draft made from the
/// previous reading becomes identifiable as a draft of an older reading, and the refresh
/// status says so with the document's name.
pub async fn reprocess(
    State(state): State<AppState>,
    session: Session,
    ApiPath((partner_id, material_id)): ApiPath<(Uuid, Uuid)>,
) -> ApiResult<Json<otdel_core::model::Material>> {
    let mut tx = state.db.begin_scoped(session.bureau_id).await?;

    let Some(stored) = materials::get_in_partner(&mut tx, partner_id, material_id).await? else {
        return Err(ApiError::not_found("material not found for this partner"));
    };
    if !stored.material.status.can_reprocess() {
        return Err(ApiError::new(stored.material.status.reprocess_refusal()));
    }

    materials::requeue(&mut tx, partner_id, material_id)
        .await?
        .ok_or_else(|| ApiError::not_found("material not found for this partner"))?;
    let job = jobs::requeue_extraction(&mut tx, partner_id, material_id).await?;

    events::record(
        &mut tx,
        &events::NewEvent::new(
            EventKind::MaterialReprocessRequested,
            EventActor::Owner,
            format!(
                "владелец запросил повторное чтение материала «{}». Оригинал и уже \
                 опубликованные версии не меняются",
                stored.material.filename
            ),
        )
        .for_partner(partner_id)
        .about_material(material_id)
        .about_job(job.id)
        .with_detail(serde_json::json!({
            "previous_status": stored.material.status.as_str(),
            "content_revision": stored.material.content_revision,
        })),
    )
    .await?;

    let material = materials::get_in_partner_with_extraction(&mut tx, partner_id, material_id)
        .await?
        .ok_or_else(|| ApiError::not_found("material not found for this partner"))?;
    tx.commit().await?;

    info!(
        material_id = %material_id,
        job_id = %job.id,
        "material queued for a fresh reading by the owner"
    );
    Ok(Json(material))
}

// --- shared -----------------------------------------------------------------------------

fn version_ref(version: &KnowledgeVersion) -> VersionRef {
    VersionRef {
        id: version.id,
        number: version.number,
        status: version.status.as_str().to_owned(),
        published_at: version.published_at,
    }
}

/// How many steps ended in each outcome. Bounded by the size of the outcome vocabulary,
/// which is what keeps the event payload a fixed size whatever the partner has.
fn outcome_counts(steps: &[RefreshStep]) -> serde_json::Value {
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for step in steps {
        *counts.entry(step.outcome.as_str()).or_default() += 1;
    }
    serde_json::json!(counts)
}

/// Whether the 1C run behind a document actually produced a draft.
///
/// `drafted_revision` cannot answer this on its own, and assuming it could was a real
/// defect: the revision is written when a run **starts**, before the model is called, so a
/// run that then failed — or stopped because no model is configured — leaves a number
/// indistinguishable from success. The document was reported as «разобран по текущему
/// чтению: фактов 0», the refresh status said `current`, and `POST .../refresh` answered
/// `up_to_date`, leaving no button that would do anything about it.
fn draft_finished(run_status: Option<&str>) -> bool {
    matches!(run_status, Some("completed") | Some("partial"))
}

/// Where one document stands, from its rows alone.
///
/// The branches are ordered from most to least certain, and the last two are the ones
/// that matter: a run that did not finish is not a draft, and a draft made from reading
/// *N* while the document is at reading *N+1* is a draft of an older reading.
fn source_state(source: &updates::SourceRow) -> SourceState {
    if matches!(
        source.status,
        MaterialStatus::Queued | MaterialStatus::Processing
    ) {
        return SourceState::Reading;
    }
    if source.pages_with_text == 0 {
        return SourceState::Unreadable;
    }
    if !draft_finished(source.run_status.as_deref()) {
        // Never attempted, in flight, failed, or waiting for a model. All four need the
        // same thing done about them; `draft_status` on the wire says which it is.
        return SourceState::NotDrafted;
    }
    match source.drafted_revision {
        // A finished run that predates the column: the reading it used is unknown, and
        // unknown is reported as such rather than as current.
        None => SourceState::Drafted,
        Some(drafted) if drafted < source.content_revision => SourceState::RereadAfterDraft,
        Some(_) => SourceState::Drafted,
    }
}

fn source_message(source: &updates::SourceRow, state: SourceState) -> String {
    match state {
        SourceState::Reading => "материал читается или стоит в очереди на чтение".to_owned(),
        SourceState::Unreadable => "страниц с пригодным текстом нет: разбирать нечего".to_owned(),
        // Four different situations wear this state, and the owner needs to know which.
        SourceState::NotDrafted => match source.run_status.as_deref() {
            None => "материал прочитан, но ни разу не разбирался продуктологом".to_owned(),
            Some("queued") => "разбор поставлен в очередь и ещё не выполнялся".to_owned(),
            Some("running") => "разбор выполняется прямо сейчас".to_owned(),
            Some("needs_provider") => {
                "разбор не запускался: модель продуктолога не настроена".to_owned()
            }
            Some(other) => format!(
                "предыдущий разбор завершился со статусом `{other}` и кандидатов не оставил: \
                 материал нужно разобрать заново"
            ),
        },
        SourceState::RereadAfterDraft => format!(
            "разбор сделан по чтению №{}, с тех пор документ перечитывался (№{}): кандидаты \
             могли быть сделаны по другому тексту — проверка перечитает цитаты и решит",
            source.drafted_revision.unwrap_or_default(),
            source.content_revision
        ),
        SourceState::Drafted => format!(
            "разобран по текущему чтению №{}: фактов {}, из них в опубликованной версии {}",
            source.content_revision, source.facts_drafted, source.claims_in_published
        ),
    }
}

/// The reason this document contributes to "нужна перепроверка", if it does.
fn source_reason(source: &updates::SourceRow, state: SourceState) -> Option<RefreshReason> {
    let code = match state {
        SourceState::Reading => RefreshReasonCode::MaterialNotRead,
        SourceState::NotDrafted => RefreshReasonCode::MaterialNotDrafted,
        SourceState::RereadAfterDraft => RefreshReasonCode::SourceReread,
        // A document nothing could be read from is a fact about that document, not a
        // reason to check again: checking would read the same absence.
        SourceState::Unreadable | SourceState::Drafted => return None,
    };
    Some(RefreshReason {
        code,
        message: source_message(source, state),
        material_id: Some(source.material_id),
        material_filename: Some(source.filename.clone()),
        version_id: None,
        version_number: None,
        content_revision: Some(source.content_revision),
        drafted_revision: source.drafted_revision,
    })
}

fn describe(state: RefreshState, reasons: &[RefreshReason]) -> String {
    match state {
        RefreshState::Checking => {
            "Проверка идёт. Опубликованная версия остаётся прежней, пока проверка не \
             закончится."
                .to_owned()
        }
        RefreshState::Current => {
            "Опубликованная версия построена из тех же кандидатов, что есть сейчас.".to_owned()
        }
        RefreshState::NeverPublished => "Опубликованной версии ещё нет. Причины — ниже.".to_owned(),
        RefreshState::Retracted => {
            "Опубликованная версия отозвана. Поиск и ответы по ней недоступны, пока не \
             будет опубликована новая."
                .to_owned()
        }
        RefreshState::RevalidationRequired => format!(
            "Нужна перепроверка: причин — {}. Опубликованная версия не меняется, пока \
             новая не пройдёт правила.",
            reasons.len()
        ),
    }
}

//! PostgreSQL layer for OTDEL phase 1A.
//!
//! Two roles are used and they are not interchangeable:
//!
//! * the **migration role** owns the schema and is only used by `otdel-api migrate`;
//! * the **runtime role** (`otdel_app`) has no ownership, no `BYPASSRLS`, and every
//!   tenant table is under forced row-level security for it.
//!
//! Because of that, all tenant queries go through [`Database::begin_scoped`], which opens
//! a transaction and sets the bureau context *inside* it (`set_config(..., local)`), so a
//! pooled connection can never leak a context into the next request. See
//! [`tenancy`] for details.

pub mod error;
pub mod jobs;
pub mod knowledge;
pub mod knowledge_read;
pub mod materials;
pub mod pages;
pub mod partners;
pub mod publication;
pub mod publication_read;
pub mod research;
pub mod research_read;
pub mod retrieval;
pub mod sessions;
pub mod tenancy;

use std::time::Duration;

use log::LevelFilter;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool, Row};
use uuid::Uuid;

pub use error::{DbError, DbResult};
pub use tenancy::ScopedTx;

/// Embedded migrations (`migrations/` at the repository root), applied with the
/// migration role only.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// `search_path` pinned on every connection this crate opens.
///
/// All application SQL is schema-qualified. Pinning the path keeps SQLx's unqualified
/// `_sqlx_migrations` bookkeeping table in one predictable schema (`public`) instead of
/// following whatever default the role happens to have — see [`run_migrations`].
const MIGRATION_SEARCH_PATH: &str = "public";

/// Tenant tables whose row-level security is verified before the server serves a
/// request. Grows with the schema: 1A intake, 1B page evidence, 1C product draft,
/// 1D research money and external sources.
const TENANT_TABLES: [&str; 29] = [
    "partners",
    "materials",
    "jobs",
    "material_pages",
    "page_regions",
    "table_cells",
    "knowledge_runs",
    "product_categories",
    "products",
    "knowledge_facts",
    "knowledge_evidence",
    "glossary_terms",
    "knowledge_qa",
    "knowledge_gaps",
    "knowledge_questions",
    "research_budgets",
    "research_plans",
    "research_queries",
    "research_sources",
    "research_findings",
    "research_evidence",
    "research_spend",
    "validation_runs",
    "knowledge_versions",
    "version_claims",
    "version_evidence",
    "version_gaps",
    "version_readiness",
    "version_chunks",
];

#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Connect the runtime pool.
    ///
    /// Statement logging is disabled: query text with bound parameters would put
    /// tenant data and session fingerprints into the log.
    pub async fn connect(url: &str, max_connections: u32) -> DbResult<Self> {
        let options: PgConnectOptions = url
            .parse::<PgConnectOptions>()?
            // Fixed `search_path` on every connection of the pool. All application SQL
            // is schema-qualified (`otdel.partners`, …), so nothing depends on the
            // role's default; pinning it means a changed role default cannot silently
            // redirect an unqualified name somewhere else.
            .options([("search_path", MIGRATION_SEARCH_PATH)])
            .log_statements(LevelFilter::Off)
            .log_slow_statements(LevelFilter::Warn, Duration::from_secs(2));

        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Refuse to run the API with a privileged database role.
    ///
    /// Naming a role in the migration is not enough: what matters is the role this pool
    /// is actually connected as. A superuser, a `BYPASSRLS` role or the owner of the
    /// tenant tables would silently make every row-level-security policy inert, so the
    /// server checks the real situation at startup and refuses to serve otherwise.
    ///
    /// `row_security_active` is the decisive check: it answers “are policies applied to
    /// *me* on this table right now”, covering both privileges and `FORCE ROW LEVEL
    /// SECURITY`.
    pub async fn verify_runtime_role(&self) -> DbResult<()> {
        // Every tenant table, checked by name. A table added by a later migration that
        // is not listed here would be isolated by its policy but unverified at startup,
        // so the list grows with the schema.
        let checks: String = TENANT_TABLES
            .iter()
            .enumerate()
            .map(|(index, table)| format!(", row_security_active('otdel.{table}') AS rls_{index}"))
            .collect();

        let row = sqlx::query(&format!(
            "SELECT current_user AS role_name, \
                    r.rolsuper AS is_superuser, \
                    r.rolbypassrls AS bypasses_rls, \
                    r.rolcreaterole AS can_create_role, \
                    r.rolcreatedb AS can_create_db, \
                    EXISTS ( \
                        SELECT 1 FROM pg_class c \
                          JOIN pg_namespace n ON n.oid = c.relnamespace \
                         WHERE n.nspname = 'otdel' AND c.relkind = 'r' AND c.relowner = r.oid \
                    ) AS owns_tables, \
                    has_table_privilege(current_user, 'otdel.sessions', 'SELECT') AS reads_sessions \
                    {checks} \
               FROM pg_roles r WHERE r.rolname = current_user"
        ))
        .fetch_one(&self.pool)
        .await?;

        let role_name: String = row.try_get("role_name")?;
        let mut problems: Vec<String> = Vec::new();
        for (column, problem) in [
            ("is_superuser", "the role is SUPERUSER"),
            ("bypasses_rls", "the role has BYPASSRLS"),
            ("can_create_role", "the role has CREATEROLE"),
            ("can_create_db", "the role has CREATEDB"),
            ("owns_tables", "the role owns tables in the otdel schema"),
            (
                "reads_sessions",
                "the role can read otdel.sessions directly",
            ),
        ] {
            if row.try_get::<bool, _>(column)? {
                problems.push(problem.to_owned());
            }
        }

        // Every tenant table, including the 1B evidence tables, the 1C draft and the 1D
        // research journal: a quoted fragment of a catalogue is partner data exactly as
        // much as the original file is, and so is the list of questions this bureau paid
        // to have answered.
        for (index, table) in TENANT_TABLES.iter().enumerate() {
            if !row.try_get::<bool, _>(format!("rls_{index}").as_str())? {
                problems.push(format!(
                    "row-level security is not applied on otdel.{table}"
                ));
            }
        }

        if problems.is_empty() {
            tracing::info!(
                database_role = %role_name,
                tables_checked = TENANT_TABLES.len(),
                "runtime database role verified: row-level security applies to it"
            );
            return Ok(());
        }

        Err(DbError::UnsafeRuntimeRole {
            role: role_name,
            problems,
        })
    }

    /// Readiness probe.
    pub async fn health(&self) -> DbResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Resolve the configured bureau. `None` means the database is migrated but the
    /// bureau was never provisioned.
    pub async fn bureau_id_by_slug(&self, slug: &str) -> DbResult<Option<Uuid>> {
        let row = sqlx::query("SELECT otdel.bureau_id_by_slug($1) AS id")
            .bind(slug)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<Option<Uuid>, _>("id")?)
    }

    /// Honest report of pgvector availability.
    ///
    /// Phase 1A stores no embeddings, so nothing here depends on the extension. The
    /// probe exists so `/ready` can state the real situation instead of implying that
    /// semantic search is ready: `Installed`, `Available` (present in the image, not yet
    /// created) or `Absent` (the image does not ship it).
    pub async fn pgvector_status(&self) -> DbResult<PgVectorStatus> {
        let row = sqlx::query(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector') AS installed, \
                    EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'vector') AS available",
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(if row.try_get::<bool, _>("installed")? {
            PgVectorStatus::Installed
        } else if row.try_get::<bool, _>("available")? {
            PgVectorStatus::Available
        } else {
            PgVectorStatus::Absent
        })
    }

    /// Open a transaction bound to one bureau. Every tenant query must use this.
    pub async fn begin_scoped(&self, bureau_id: Uuid) -> DbResult<ScopedTx> {
        ScopedTx::begin(&self.pool, bureau_id).await
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// State of the `vector` extension in the connected database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgVectorStatus {
    /// `CREATE EXTENSION vector` has been executed.
    Installed,
    /// The extension is shipped by the server image but not created yet.
    Available,
    /// The server image does not provide it.
    Absent,
}

impl PgVectorStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Available => "available_not_installed",
            Self::Absent => "absent",
        }
    }
}

/// Provision a bureau with the **migration** role (idempotent).
///
/// The runtime role cannot do this: it has no privileges on `otdel.bureaus` at all.
pub async fn bootstrap_bureau(admin_url: &str, slug: &str) -> DbResult<Uuid> {
    let pool = admin_pool(admin_url).await?;
    let result = sqlx::query(
        "WITH created AS ( \
             INSERT INTO otdel.bureaus (slug, name) VALUES ($1, $2) \
             ON CONFLICT (slug) DO NOTHING RETURNING id \
         ) \
         SELECT id FROM created \
         UNION ALL \
         SELECT id FROM otdel.bureaus WHERE slug = $1 \
         LIMIT 1",
    )
    .bind(slug)
    .bind(format!("Бюро {slug}"))
    .fetch_one(&pool)
    .await;
    pool.close().await;

    result?.try_get::<Uuid, _>("id").map_err(DbError::from)
}

async fn admin_pool(admin_url: &str) -> DbResult<PgPool> {
    let options: PgConnectOptions = admin_url
        .parse::<PgConnectOptions>()?
        .options([("search_path", MIGRATION_SEARCH_PATH)])
        .log_statements(LevelFilter::Off);
    PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .map_err(DbError::from)
}

/// Apply migrations with the **migration** role.
///
/// Re-running this must be a no-op, and that depends on one subtle thing: SQLx records
/// applied versions in an **unqualified** `_sqlx_migrations` table, so which schema that
/// table lands in is decided by `search_path`. The migration role's default search path
/// is `otdel, public`, which means the first run (before the `otdel` schema exists)
/// writes the history into `public`, while a later run would resolve to a *new, empty*
/// `otdel._sqlx_migrations` and replay every migration — failing with “function
/// current_bureau_id already exists”.
///
/// Pinning `search_path` on the connection makes `public._sqlx_migrations` the one
/// canonical history. Every statement in `migrations/` is schema-qualified, so nothing
/// else depends on the search path.
pub async fn run_migrations(admin_url: &str) -> DbResult<()> {
    let pool = admin_pool(admin_url).await?;

    let result = async {
        remove_stray_migration_table(&pool).await?;
        MIGRATOR.run(&pool).await.map_err(DbError::from)
    }
    .await;

    pool.close().await;
    result
}

/// Drop an **empty** `otdel._sqlx_migrations` left behind by a run that used the role's
/// default search path.
///
/// Only the empty table is removed, and only in the `otdel` schema: a table with rows is
/// somebody's real history and is left untouched (with a warning) rather than guessed
/// about. Nothing else in the database is touched — no schema, table or row of the pilot
/// data is dropped here.
async fn remove_stray_migration_table(pool: &PgPool) -> DbResult<()> {
    let Some(row) = sqlx::query(
        "SELECT count(*) AS rows_present FROM otdel._sqlx_migrations \
         WHERE to_regclass('otdel._sqlx_migrations') IS NOT NULL",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten() else {
        // The table does not exist (the query fails to parse against a missing relation),
        // which is the normal, healthy case.
        return Ok(());
    };

    let rows_present: i64 = row.try_get("rows_present")?;
    if rows_present > 0 {
        tracing::warn!(
            rows = rows_present,
            "otdel._sqlx_migrations exists and is not empty; leaving it alone. \
             The canonical history is public._sqlx_migrations"
        );
        return Ok(());
    }

    sqlx::query("DROP TABLE IF EXISTS otdel._sqlx_migrations")
        .execute(pool)
        .await?;
    tracing::info!("removed an empty stray otdel._sqlx_migrations table");
    Ok(())
}

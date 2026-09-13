-- Bootstrap of the single local bureau (workspace) of the pilot.
--
-- The local pilot has one owner and one bureau; there is no open registration. The slug
-- is fixed here and must match `OTDEL_BUREAU_SLUG` (default `local`); `otdel-api
-- bootstrap` can provision an additional bureau slug for a second local workspace.
INSERT INTO otdel.bureaus (slug, name)
VALUES ('local', 'Локальное бюро')
ON CONFLICT (slug) DO NOTHING;

use mica_driver::DriverAdministrator;
use mica_var::{Symbol, Value};

use crate::{hash_password, verify_password};

fn mica_escape(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}

fn stable_subject_symbol(prefix: &str, provider: &str, provider_sub: &str) -> String {
    fn push_sanitized(out: &mut String, value: &str) {
        let mut last_was_separator = false;
        for c in value.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
                last_was_separator = false;
            } else if !last_was_separator {
                out.push('_');
                last_was_separator = true;
            }
        }
        while out.ends_with('_') {
            out.pop();
        }
    }

    let mut symbol = prefix.to_owned();
    symbol.push('_');
    push_sanitized(&mut symbol, provider);
    symbol.push('_');
    push_sanitized(&mut symbol, provider_sub);
    symbol
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthRoleSymbols {
    pub admin: String,
    pub operator: String,
    pub viewer: String,
}

impl AuthRoleSymbols {
    pub fn namespaced(namespace: &str) -> Self {
        Self {
            admin: format!("{namespace}/role_admin"),
            operator: format!("{namespace}/role_operator"),
            viewer: format!("{namespace}/role_viewer"),
        }
    }

    fn symbol_for_role(&self, role: &str) -> Option<&str> {
        match role.trim().to_ascii_lowercase().as_str() {
            "admin" => Some(&self.admin),
            "operator" => Some(&self.operator),
            "viewer" => Some(&self.viewer),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthSchema {
    pub auth_session: String,
    pub session_user: String,
    pub session_actor: String,
    pub session_provider: String,
    pub session_provider_sub: String,
    pub session_issued_at: String,
    pub session_expires_at: String,
    pub session_revoked_at: String,
    pub session_last_seen_at: String,
    pub user: String,
    pub user_external_identity: String,
    pub user_provider: String,
    pub user_login: String,
    pub user_role: String,
    pub local_password_hash: String,
    pub person: String,
    pub user_person: String,
    pub default_user_person: String,
    pub display_name: String,
    pub description: String,
    pub user_identity_prefix: String,
    pub person_identity_prefix: String,
    pub roles: AuthRoleSymbols,
}

impl AuthSchema {
    pub fn namespaced(namespace: &str) -> Self {
        Self {
            auth_session: format!("{namespace}/AuthSession"),
            session_user: format!("{namespace}/SessionUser"),
            session_actor: format!("{namespace}/SessionActor"),
            session_provider: format!("{namespace}/SessionProvider"),
            session_provider_sub: format!("{namespace}/SessionProviderSub"),
            session_issued_at: format!("{namespace}/SessionIssuedAt"),
            session_expires_at: format!("{namespace}/SessionExpiresAt"),
            session_revoked_at: format!("{namespace}/SessionRevokedAt"),
            session_last_seen_at: format!("{namespace}/SessionLastSeenAt"),
            user: format!("{namespace}/User"),
            user_external_identity: format!("{namespace}/UserExternalIdentity"),
            user_provider: format!("{namespace}/UserProvider"),
            user_login: format!("{namespace}/UserLogin"),
            user_role: format!("{namespace}/UserRole"),
            local_password_hash: format!("{namespace}/LocalPasswordHash"),
            person: format!("{namespace}/Person"),
            user_person: format!("{namespace}/UserPerson"),
            default_user_person: format!("{namespace}/DefaultUserPerson"),
            display_name: format!("{namespace}/DisplayName"),
            description: format!("{namespace}/Description"),
            user_identity_prefix: format!("{namespace}/user"),
            person_identity_prefix: format!("{namespace}/person"),
            roles: AuthRoleSymbols::namespaced(namespace),
        }
    }

    fn stable_user_symbol(&self, provider: &str, provider_sub: &str) -> String {
        stable_subject_symbol(&self.user_identity_prefix, provider, provider_sub)
    }

    fn stable_person_symbol(&self, provider: &str, provider_sub: &str) -> String {
        stable_subject_symbol(&self.person_identity_prefix, provider, provider_sub)
    }
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub session_id: String,
    pub user_id: String,
    pub actor: String,
    pub provider: String,
    pub provider_sub: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub last_seen_at: i64,
    pub roles_version: Option<u64>,
    pub user_agent_hash: Option<String>,
}

pub struct MicaSessionStore {
    administrator: DriverAdministrator,
    schema: AuthSchema,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalAuthenticatedUser {
    pub user_id: String,
    pub provider_sub: String,
    pub login: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LocalUserCreateError {
    AlreadyExists(String),
    Store(String),
}

impl std::fmt::Display for LocalUserCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(login) => write!(f, "local user {login:?} already exists"),
            Self::Store(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for LocalUserCreateError {}

impl MicaSessionStore {
    pub fn new(administrator: DriverAdministrator, schema: AuthSchema) -> Self {
        Self {
            administrator,
            schema,
        }
    }

    pub async fn create_session(&self, record: &SessionRecord) -> Result<(), String> {
        let schema = &self.schema;
        let escaped_user_id = mica_escape(&record.user_id);
        let escaped_actor = mica_escape(&record.actor);
        let escaped_provider = mica_escape(&record.provider);
        let escaped_provider_sub = mica_escape(&record.provider_sub);
        let source = format!(
            r#"
let session_id = to_symbol("{session_id}")
let user_id = to_symbol("{escaped_user_id}")
let actor_id = to_symbol("{escaped_actor}")
retract {auth_session}(session_id)
assert {auth_session}(session_id)
retract {session_user}(session_id, _)
assert {session_user}(session_id, user_id)
retract {session_actor}(session_id, _)
assert {session_actor}(session_id, actor_id)
retract {session_provider}(session_id, _)
assert {session_provider}(session_id, to_symbol("{escaped_provider}"))
retract {session_provider_sub}(session_id, _)
assert {session_provider_sub}(session_id, "{escaped_provider_sub}")
retract {session_issued_at}(session_id, _)
assert {session_issued_at}(session_id, {issued_at})
retract {session_expires_at}(session_id, _)
assert {session_expires_at}(session_id, {expires_at})
retract {session_revoked_at}(session_id, _)
retract {session_last_seen_at}(session_id, _)
assert {session_last_seen_at}(session_id, {last_seen_at})
return true
"#,
            auth_session = schema.auth_session,
            session_user = schema.session_user,
            session_actor = schema.session_actor,
            session_provider = schema.session_provider,
            session_provider_sub = schema.session_provider_sub,
            session_issued_at = schema.session_issued_at,
            session_expires_at = schema.session_expires_at,
            session_revoked_at = schema.session_revoked_at,
            session_last_seen_at = schema.session_last_seen_at,
            session_id = record.session_id,
            escaped_user_id = escaped_user_id,
            escaped_actor = escaped_actor,
            escaped_provider = escaped_provider,
            escaped_provider_sub = escaped_provider_sub,
            issued_at = record.issued_at,
            expires_at = record.expires_at,
            last_seen_at = record.last_seen_at,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to create session: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { .. } => Ok(()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("create session aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("create session suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn lookup_session(&self, session_id: &str) -> Result<Option<SessionRecord>, String> {
        let schema = &self.schema;
        let source = format!(
            r#"
let session_id = to_symbol("{session_id}")
{auth_session}(session_id) || return nothing
let user = one {session_user}(session_id, ?user)
let actor = one {session_actor}(session_id, ?actor)
let provider = one {session_provider}(session_id, ?provider)
let provider_sub = one {session_provider_sub}(session_id, ?provider_sub)
let issued_at = one {session_issued_at}(session_id, ?issued_at)
let expires_at = one {session_expires_at}(session_id, ?expires_at)
let revoked_at = one {session_revoked_at}(session_id, ?revoked_at)
let last_seen_at = one {session_last_seen_at}(session_id, ?last_seen_at)
return {{:session_id -> "{session_id}", :user_id -> user, :actor -> actor, :provider -> provider, :provider_sub -> provider_sub, :issued_at -> issued_at, :expires_at -> expires_at, :revoked_at -> revoked_at, :last_seen_at -> last_seen_at}}
"#,
            auth_session = schema.auth_session,
            session_user = schema.session_user,
            session_actor = schema.session_actor,
            session_provider = schema.session_provider,
            session_provider_sub = schema.session_provider_sub,
            session_issued_at = schema.session_issued_at,
            session_expires_at = schema.session_expires_at,
            session_revoked_at = schema.session_revoked_at,
            session_last_seen_at = schema.session_last_seen_at,
            session_id = session_id,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to lookup session: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { value, .. } => {
                if value == Value::nothing() {
                    return Ok(None);
                }
                let map = &value;
                let user_id = map_string(map, "user_id")?;
                let actor = map_string(map, "actor")?;
                let provider = map_string(map, "provider")?;
                let provider_sub = map_string(map, "provider_sub")?;
                let issued_at = map_int(map, "issued_at")?;
                let expires_at = map_int(map, "expires_at")?;
                let revoked_at = map_optional_int(map, "revoked_at")?;
                let last_seen_at = map_int(map, "last_seen_at")?;

                Ok(Some(SessionRecord {
                    session_id: session_id.to_owned(),
                    user_id,
                    actor,
                    provider,
                    provider_sub,
                    issued_at,
                    expires_at,
                    revoked_at,
                    last_seen_at,
                    roles_version: None,
                    user_agent_hash: None,
                }))
            }
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("lookup session aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("lookup session suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn revoke_session(&self, session_id: &str) -> Result<(), String> {
        let schema = &self.schema;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let source = format!(
            r#"
let session_id = to_symbol("{session_id}")
{auth_session}(session_id) || return false
retract {session_revoked_at}(session_id, _)
assert {session_revoked_at}(session_id, {now})
return true
"#,
            auth_session = schema.auth_session,
            session_revoked_at = schema.session_revoked_at,
            session_id = session_id,
            now = now,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to revoke session: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { .. } => Ok(()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("revoke session aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("revoke session suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn update_last_seen(&self, session_id: &str) -> Result<(), String> {
        let schema = &self.schema;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let source = format!(
            r#"
let session_id = to_symbol("{session_id}")
{auth_session}(session_id) || return false
retract {session_last_seen_at}(session_id, _)
assert {session_last_seen_at}(session_id, {now})
return true
"#,
            auth_session = schema.auth_session,
            session_last_seen_at = schema.session_last_seen_at,
            session_id = session_id,
            now = now,
        );
        let _ = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to update last seen: {e}"))?;
        Ok(())
    }

    pub async fn ensure_user_exists(
        &self,
        login: &str,
        provider: &str,
        provider_sub: &str,
    ) -> Result<String, String> {
        let escaped_login = mica_escape(login);
        let escaped_provider = mica_escape(provider);
        let escaped_provider_sub = mica_escape(provider_sub);
        let schema = &self.schema;
        let user_symbol = schema.stable_user_symbol(provider, provider_sub);
        let escaped_user_symbol = mica_escape(&user_symbol);
        let source = format!(
            r#"
let provider = to_symbol("{escaped_provider}")
let provider_sub = "{escaped_provider_sub}"
let user_symbol = to_symbol("{escaped_user_symbol}")
let identity = make_identity(user_symbol)
{user_external_identity}(provider, provider_sub, identity) || assert {user_external_identity}(provider, provider_sub, identity)
assert {user}(identity)
retract {user_provider}(identity, _)
assert {user_provider}(identity, provider)
retract {user_login}(identity, _)
assert {user_login}(identity, "{escaped_login}")
{user_role}(identity, :{viewer_role}) || assert {user_role}(identity, :{viewer_role})
return "{escaped_user_symbol}"
"#,
            user_external_identity = schema.user_external_identity,
            user = schema.user,
            user_provider = schema.user_provider,
            user_login = schema.user_login,
            user_role = schema.user_role,
            viewer_role = schema.roles.viewer,
            escaped_login = escaped_login,
            escaped_provider = escaped_provider,
            escaped_provider_sub = escaped_provider_sub,
            escaped_user_symbol = escaped_user_symbol,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to ensure user exists: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { value, .. } => value
                .with_str(str::to_owned)
                .ok_or_else(|| "ensure user returned non-string identity name".to_owned()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("ensure user aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("ensure user suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn ensure_user_person(
        &self,
        user_id: &str,
        provider: &str,
        provider_sub: &str,
        display_name: &str,
    ) -> Result<String, String> {
        let escaped_user_id = mica_escape(user_id);
        let escaped_display_name = mica_escape(display_name);
        let schema = &self.schema;
        let person_symbol = schema.stable_person_symbol(provider, provider_sub);
        let escaped_person_symbol = mica_escape(&person_symbol);
        let source = format!(
            r#"
let user = make_identity(to_symbol("{escaped_user_id}"))
let person_symbol = to_symbol("{escaped_person_symbol}")
let current_default = one {default_user_person}(user, ?person)
let actor = current_default
let actor_symbol = ""
if current_default != nothing
  actor_symbol = string_slice(to_literal(current_default), 1, string_len(to_literal(current_default)))
else
  let person = make_identity(person_symbol)
  assert {person}(person)
  {user_person}(user, person) || assert {user_person}(user, person)
  assert {default_user_person}(user, person)
  actor = person
  actor_symbol = "{escaped_person_symbol}"
end
{user_person}(user, actor) || assert {user_person}(user, actor)
retract {display_name}(actor, _)
assert {display_name}(actor, "{escaped_display_name}")
retract {description}(actor, _)
assert {description}(actor, "{escaped_display_name}, present through authenticated login.")
return actor_symbol
"#,
            person = schema.person,
            user_person = schema.user_person,
            default_user_person = schema.default_user_person,
            display_name = schema.display_name,
            description = schema.description,
            escaped_user_id = escaped_user_id,
            escaped_person_symbol = escaped_person_symbol,
            escaped_display_name = escaped_display_name,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to ensure user person exists: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { value, .. } => value
                .with_str(str::to_owned)
                .ok_or_else(|| "ensure user person returned non-string identity name".to_owned()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("ensure user person aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("ensure user person suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn create_local_user(
        &self,
        login: &str,
        password: &str,
        display_name: &str,
    ) -> Result<LocalAuthenticatedUser, LocalUserCreateError> {
        let provider_sub = normalize_local_login(login).map_err(LocalUserCreateError::Store)?;
        if self
            .lookup_local_user_id(&provider_sub)
            .await
            .map_err(LocalUserCreateError::Store)?
            .is_some()
        {
            return Err(LocalUserCreateError::AlreadyExists(provider_sub));
        }
        self.upsert_local_user_with_provider_sub(login, password, display_name, &provider_sub)
            .await
            .map_err(LocalUserCreateError::Store)
    }

    pub async fn upsert_local_user(
        &self,
        login: &str,
        password: &str,
        display_name: &str,
    ) -> Result<LocalAuthenticatedUser, String> {
        let provider_sub = normalize_local_login(login)?;
        self.upsert_local_user_with_provider_sub(login, password, display_name, &provider_sub)
            .await
    }

    async fn upsert_local_user_with_provider_sub(
        &self,
        login: &str,
        password: &str,
        display_name: &str,
        provider_sub: &str,
    ) -> Result<LocalAuthenticatedUser, String> {
        let user_id = self
            .ensure_user_exists(login, "local", provider_sub)
            .await?;
        let password_hash =
            hash_password(password).map_err(|error| format!("failed to hash password: {error}"))?;
        self.set_local_password_hash(&user_id, &password_hash)
            .await?;
        self.ensure_user_person(&user_id, "local", provider_sub, display_name)
            .await?;
        Ok(LocalAuthenticatedUser {
            user_id,
            provider_sub: provider_sub.to_owned(),
            login: login.to_owned(),
        })
    }

    async fn lookup_local_user_id(&self, provider_sub: &str) -> Result<Option<String>, String> {
        let schema = &self.schema;
        let escaped_provider_sub = mica_escape(provider_sub);
        let source = format!(
            r#"
let user = one {user_external_identity}(:local, "{escaped_provider_sub}", ?user)
user != nothing || return nothing
return string_slice(to_literal(user), 1, string_len(to_literal(user)))
"#,
            user_external_identity = schema.user_external_identity,
            escaped_provider_sub = escaped_provider_sub,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to lookup local user: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { value, .. } => {
                if value == Value::nothing() {
                    return Ok(None);
                }
                value
                    .with_str(str::to_owned)
                    .map(Some)
                    .ok_or_else(|| "lookup local user returned non-string identity name".to_owned())
            }
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("lookup local user aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("lookup local user suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn grant_user_role(&self, user_id: &str, role: &str) -> Result<(), String> {
        let schema = &self.schema;
        let role_symbol = schema.roles.symbol_for_role(role).ok_or_else(|| {
            format!(
                "unsupported auth role {:?}",
                role.trim().to_ascii_lowercase()
            )
        })?;
        let escaped_user_id = mica_escape(user_id);
        let source = format!(
            r#"
let user = make_identity(to_symbol("{escaped_user_id}"))
{user_role}(user, :{role_symbol}) || assert {user_role}(user, :{role_symbol})
return true
"#,
            user_role = schema.user_role,
            escaped_user_id = escaped_user_id,
            role_symbol = role_symbol,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to grant user role: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { .. } => Ok(()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("grant user role aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("grant user role suspended unexpectedly".to_owned())
            }
        }
    }

    async fn set_local_password_hash(
        &self,
        user_id: &str,
        password_hash: &str,
    ) -> Result<(), String> {
        let schema = &self.schema;
        let escaped_user_id = mica_escape(user_id);
        let escaped_password_hash = mica_escape(password_hash);
        let source = format!(
            r#"
let user = make_identity(to_symbol("{escaped_user_id}"))
retract {local_password_hash}(user, _)
assert {local_password_hash}(user, "{escaped_password_hash}")
return true
"#,
            local_password_hash = schema.local_password_hash,
            escaped_user_id = escaped_user_id,
            escaped_password_hash = escaped_password_hash,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to set local password hash: {e}"))?;
        match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { .. } => Ok(()),
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                Err(format!("set local password hash aborted: {error}"))
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                Err("set local password hash suspended unexpectedly".to_owned())
            }
        }
    }

    pub async fn authenticate_local_user(
        &self,
        login: &str,
        password: &str,
    ) -> Result<Option<LocalAuthenticatedUser>, String> {
        let provider_sub = normalize_local_login(login)?;
        let schema = &self.schema;
        let user_symbol = schema.stable_user_symbol("local", &provider_sub);
        let escaped_provider_sub = mica_escape(&provider_sub);
        let escaped_user_symbol = mica_escape(&user_symbol);
        let source = format!(
            r#"
let user = one {user_external_identity}(:local, "{escaped_provider_sub}", ?user)
user != nothing || return nothing
let password_hash = one {local_password_hash}(user, ?password_hash)
password_hash != nothing || return nothing
return {{:user_id -> "{escaped_user_symbol}", :password_hash -> password_hash}}
"#,
            user_external_identity = schema.user_external_identity,
            local_password_hash = schema.local_password_hash,
            escaped_provider_sub = escaped_provider_sub,
            escaped_user_symbol = escaped_user_symbol,
        );
        let report = self
            .administrator
            .evaluate(source)
            .await
            .map_err(|e| format!("failed to authenticate local user: {e}"))?;
        let map = match report.initial_report().outcome.clone() {
            mica_runtime::TaskOutcome::Complete { value, .. } => {
                if value == Value::nothing() {
                    return Ok(None);
                }
                value
            }
            mica_runtime::TaskOutcome::Aborted { error, .. } => {
                return Err(format!("authenticate local user aborted: {error}"));
            }
            mica_runtime::TaskOutcome::Suspended { .. } => {
                return Err("authenticate local user suspended unexpectedly".to_owned());
            }
        };
        let user_id = map_string(&map, "user_id")?;
        let password_hash = map_string(&map, "password_hash")?;
        let valid = verify_password(password, &password_hash)
            .map_err(|error| format!("failed to verify password: {error}"))?;
        if !valid {
            return Ok(None);
        }
        Ok(Some(LocalAuthenticatedUser {
            user_id,
            provider_sub,
            login: login.to_owned(),
        }))
    }
}

fn normalize_local_login(login: &str) -> Result<String, String> {
    let normalized = login.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err("local login must not be empty".to_owned());
    }
    Ok(normalized)
}

fn map_string(value: &Value, key: &str) -> Result<String, String> {
    let key_val = Value::symbol(Symbol::intern(key));
    let Some(inner) = value.map_get(&key_val) else {
        return Err(format!("missing session field: {key}"));
    };
    if let Some(value) = inner.with_str(str::to_owned) {
        return Ok(value);
    }
    if let Some(symbol) = inner.as_symbol().and_then(Symbol::name) {
        return Ok(symbol.to_owned());
    }
    Err(format!("session field {key} is not a string or symbol"))
}

fn map_int(value: &Value, key: &str) -> Result<i64, String> {
    let key_val = Value::symbol(Symbol::intern(key));
    let Some(inner) = value.map_get(&key_val) else {
        return Err(format!("missing session field: {key}"));
    };
    inner
        .as_int()
        .ok_or_else(|| format!("session field {key} is not an integer"))
}

fn map_optional_int(value: &Value, key: &str) -> Result<Option<i64>, String> {
    let key_val = Value::symbol(Symbol::intern(key));
    let Some(inner) = value.map_get(&key_val) else {
        return Ok(None);
    };
    if inner == Value::nothing() {
        return Ok(None);
    }
    inner
        .as_int()
        .map(Some)
        .ok_or_else(|| format!("session field {key} is not an integer or nothing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mica_driver::{DriverEventPumpTask, DriverOwner, DriverResources};
    use mica_runtime::SourceRunner;
    use std::num::NonZeroUsize;

    struct TestStore {
        store: MicaSessionStore,
        _owner: DriverOwner,
        _event_pump: DriverEventPumpTask,
    }

    impl std::ops::Deref for TestStore {
        type Target = MicaSessionStore;

        fn deref(&self) -> &Self::Target {
            &self.store
        }
    }

    fn test_store(runner: SourceRunner, schema: AuthSchema) -> TestStore {
        let resources = DriverResources::new(NonZeroUsize::new(1).unwrap());
        let mut owner = DriverOwner::builder(resources)
            .source_runner(runner)
            .build()
            .unwrap();
        let events = owner.event_router();
        let event_pump = owner.take_event_pump().unwrap().spawn_router(events);
        TestStore {
            store: MicaSessionStore::new(owner.administrator(), schema),
            _owner: owner,
            _event_pump: event_pump,
        }
    }

    fn session_schema() -> &'static str {
        r#"
make_relation(:source/AuthSession, 1)
make_functional_relation(:source/SessionUser, 2, [0])
make_functional_relation(:source/SessionActor, 2, [0])
make_functional_relation(:source/SessionProvider, 2, [0])
make_functional_relation(:source/SessionProviderSub, 2, [0])
make_functional_relation(:source/SessionIssuedAt, 2, [0])
make_functional_relation(:source/SessionExpiresAt, 2, [0])
make_functional_relation(:source/SessionRevokedAt, 2, [0])
make_functional_relation(:source/SessionLastSeenAt, 2, [0])
make_relation(:source/User, 1)
make_functional_relation(:source/UserExternalIdentity, 3, [0, 1])
make_relation(:source/UserProvider, 2)
make_functional_relation(:source/UserLogin, 2, [0])
make_relation(:source/UserRole, 2)
make_functional_relation(:source/LocalPasswordHash, 2, [0])
make_relation(:source/Person, 1)
make_relation(:source/UserPerson, 2)
make_functional_relation(:source/DefaultUserPerson, 2, [0])
make_functional_relation(:source/DisplayName, 2, [0])
make_functional_relation(:source/Description, 2, [0])
"#
    }

    fn source_store(runner: SourceRunner) -> TestStore {
        test_store(runner, AuthSchema::namespaced("source"))
    }

    #[test]
    fn create_session_uses_unary_auth_session_relation() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);
            let record = SessionRecord {
                session_id: "session-1".to_owned(),
                user_id: "alice".to_owned(),
                actor: "alice".to_owned(),
                provider: "github".to_owned(),
                provider_sub: "1001".to_owned(),
                issued_at: 10,
                expires_at: 20,
                revoked_at: None,
                last_seen_at: 10,
                roles_version: None,
                user_agent_hash: None,
            };

            store.create_session(&record).await.unwrap();

            let loaded = store
                .lookup_session("session-1")
                .await
                .unwrap()
                .expect("session should be stored");
            assert_eq!(loaded.user_id, "alice");
            assert_eq!(loaded.actor, "alice");
            assert_eq!(loaded.provider, "github");
            assert_eq!(loaded.provider_sub, "1001");
            assert_eq!(loaded.issued_at, 10);
            assert_eq!(loaded.expires_at, 20);
            assert_eq!(loaded.revoked_at, None);
            assert_eq!(loaded.last_seen_at, 10);
        });
    }

    #[test]
    fn ensure_user_uses_provider_subject_as_stable_identity() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);

            let user_id = store
                .ensure_user_exists("alice-login", "github", "1001")
                .await
                .unwrap();

            assert_eq!(user_id, "source/user_github_1001");
            let report = store
                .administrator
                .evaluate(
                    r#"
source/User(#source/user_github_1001) || return false
source/UserExternalIdentity(:github, "1001", #source/user_github_1001) || return false
source/UserProvider(#source/user_github_1001, :github) || return false
source/UserLogin(#source/user_github_1001, "alice-login") || return false
source/UserRole(#source/user_github_1001, :source/role_viewer) || return false
return true
"#
                    .to_owned(),
                )
                .await
                .unwrap();
            assert!(matches!(
                report.initial_report().outcome.clone(),
                mica_runtime::TaskOutcome::Complete { value, .. } if value == Value::from(true)
            ));
        });
    }

    #[test]
    fn ensure_user_preserves_identity_when_login_changes() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);

            let first = store
                .ensure_user_exists("old-login", "github", "1001")
                .await
                .unwrap();
            let second = store
                .ensure_user_exists("new-login", "github", "1001")
                .await
                .unwrap();

            assert_eq!(first, second);
            let report = store
                .administrator
                .evaluate(
                    r#"
source/UserExternalIdentity(:github, "1001", #source/user_github_1001) || return false
source/UserLogin(#source/user_github_1001, "new-login") || return false
source/UserLogin(#source/user_github_1001, "old-login") && return false
return true
"#
                    .to_owned(),
                )
                .await
                .unwrap();
            assert!(matches!(
                report.initial_report().outcome.clone(),
                mica_runtime::TaskOutcome::Complete { value, .. } if value == Value::from(true)
            ));
        });
    }

    #[test]
    fn ensure_user_person_attaches_default_person() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);

            let user_id = store
                .ensure_user_exists("alice-login", "github", "1001")
                .await
                .unwrap();
            let person_id = store
                .ensure_user_person(&user_id, "github", "1001", "Alice Liddell")
                .await
                .unwrap();

            assert_eq!(person_id, "source/person_github_1001");
            let report = store
                .administrator
                .evaluate(
                    r#"
source/Person(#source/person_github_1001) || return false
source/UserPerson(#source/user_github_1001, #source/person_github_1001) || return false
source/DefaultUserPerson(#source/user_github_1001, #source/person_github_1001) || return false
source/DisplayName(#source/person_github_1001, "Alice Liddell") || return false
source/Description(#source/person_github_1001, "Alice Liddell, present through authenticated login.") || return false
return true
"#
                    .to_owned(),
                )
                .await
                .unwrap();
            assert!(matches!(
                report.initial_report().outcome.clone(),
                mica_runtime::TaskOutcome::Complete { value, .. } if value == Value::from(true)
            ));
        });
    }

    #[test]
    fn local_password_authenticates_against_stored_hash() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);

            let created = store
                .create_local_user("Alice", "correct horse", "Alice")
                .await
                .unwrap();
            assert_eq!(created.user_id, "source/user_local_alice");
            assert_eq!(created.provider_sub, "alice");

            let rejected = store
                .authenticate_local_user("alice", "wrong")
                .await
                .unwrap();
            assert_eq!(rejected, None);

            let authenticated = store
                .authenticate_local_user("ALICE", "correct horse")
                .await
                .unwrap()
                .expect("local password should authenticate");
            assert_eq!(authenticated.user_id, "source/user_local_alice");
            assert_eq!(authenticated.provider_sub, "alice");
        });
    }

    #[test]
    fn create_local_user_rejects_existing_login() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner.run_filein(session_schema()).unwrap();
            let store = source_store(runner);

            store
                .create_local_user("Alice", "correct horse", "Alice")
                .await
                .unwrap();
            let duplicate = store
                .create_local_user("alice", "different horse", "Alice")
                .await
                .unwrap_err();
            assert_eq!(
                duplicate,
                LocalUserCreateError::AlreadyExists("alice".to_owned())
            );

            let updated = store
                .upsert_local_user("alice", "different horse", "Alice")
                .await
                .unwrap();
            assert_eq!(updated.user_id, "source/user_local_alice");
            assert!(
                store
                    .authenticate_local_user("alice", "different horse")
                    .await
                    .unwrap()
                    .is_some()
            );
        });
    }

    #[test]
    fn schema_namespace_controls_relation_and_identity_names() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner
                .run_filein(
                    r#"
make_relation(:mud/AuthSession, 1)
make_functional_relation(:mud/SessionUser, 2, [0])
make_functional_relation(:mud/SessionActor, 2, [0])
make_functional_relation(:mud/SessionProvider, 2, [0])
make_functional_relation(:mud/SessionProviderSub, 2, [0])
make_functional_relation(:mud/SessionIssuedAt, 2, [0])
make_functional_relation(:mud/SessionExpiresAt, 2, [0])
make_functional_relation(:mud/SessionRevokedAt, 2, [0])
make_functional_relation(:mud/SessionLastSeenAt, 2, [0])
make_relation(:mud/User, 1)
make_functional_relation(:mud/UserExternalIdentity, 3, [0, 1])
make_relation(:mud/UserProvider, 2)
make_functional_relation(:mud/UserLogin, 2, [0])
make_relation(:mud/UserRole, 2)
make_functional_relation(:mud/LocalPasswordHash, 2, [0])
make_relation(:mud/Person, 1)
make_relation(:mud/UserPerson, 2)
make_functional_relation(:mud/DefaultUserPerson, 2, [0])
make_functional_relation(:mud/DisplayName, 2, [0])
make_functional_relation(:mud/Description, 2, [0])
"#,
                )
                .unwrap();
            let store = test_store(runner, AuthSchema::namespaced("mud"));

            let user_id = store
                .ensure_user_exists("alice", "github", "1001")
                .await
                .unwrap();
            let person_id = store
                .ensure_user_person(&user_id, "github", "1001", "Alice")
                .await
                .unwrap();

            assert_eq!(user_id, "mud/user_github_1001");
            assert_eq!(person_id, "mud/person_github_1001");
            let report = store
                .administrator
                .evaluate(
                    r#"
mud/User(#mud/user_github_1001) || return false
mud/UserRole(#mud/user_github_1001, :mud/role_viewer) || return false
mud/Person(#mud/person_github_1001) || return false
mud/UserPerson(#mud/user_github_1001, #mud/person_github_1001) || return false
return true
"#
                    .to_owned(),
                )
                .await
                .unwrap();
            assert!(matches!(
                report.initial_report().outcome.clone(),
                mica_runtime::TaskOutcome::Complete { value, .. } if value == Value::from(true)
            ));
        });
    }

    #[test]
    fn ensure_user_person_returns_configured_default_person() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut runner = SourceRunner::new_empty();
            runner
                .run_filein(
                    r#"
make_identity(:alice)
make_relation(:mud/AuthSession, 1)
make_functional_relation(:mud/SessionUser, 2, [0])
make_functional_relation(:mud/SessionActor, 2, [0])
make_functional_relation(:mud/SessionProvider, 2, [0])
make_functional_relation(:mud/SessionProviderSub, 2, [0])
make_functional_relation(:mud/SessionIssuedAt, 2, [0])
make_functional_relation(:mud/SessionExpiresAt, 2, [0])
make_functional_relation(:mud/SessionRevokedAt, 2, [0])
make_functional_relation(:mud/SessionLastSeenAt, 2, [0])
make_relation(:mud/User, 1)
make_functional_relation(:mud/UserExternalIdentity, 3, [0, 1])
make_relation(:mud/UserProvider, 2)
make_functional_relation(:mud/UserLogin, 2, [0])
make_relation(:mud/UserRole, 2)
make_functional_relation(:mud/LocalPasswordHash, 2, [0])
make_relation(:mud/Person, 1)
make_relation(:mud/UserPerson, 2)
make_functional_relation(:mud/DefaultUserPerson, 2, [0])
make_functional_relation(:mud/DisplayName, 2, [0])
make_functional_relation(:mud/Description, 2, [0])
"#,
                )
                .unwrap();
            let store = test_store(runner, AuthSchema::namespaced("mud"));

            let user_id = store
                .ensure_user_exists("alice", "local", "alice")
                .await
                .unwrap();
            store
                .administrator
                .evaluate(
                    r#"
assert mud/DefaultUserPerson(#mud/user_local_alice, #alice)
"#
                    .to_owned(),
                )
                .await
                .unwrap();
            let person_id = store
                .ensure_user_person(&user_id, "local", "alice", "Alice")
                .await
                .unwrap();

            assert_eq!(person_id, "alice");
            let report = store
                .administrator
                .evaluate(
                    r##"
mud/DisplayName(#alice, "Alice") || return false
mud/UserPerson(#mud/user_local_alice, #alice) || return false
let generated = from_literal("#mud/person_local_alice")
generated != nothing && mud/UserPerson(#mud/user_local_alice, generated) && return false
return true
"##
                    .to_owned(),
                )
                .await
                .unwrap();
            assert!(matches!(
                report.initial_report().outcome.clone(),
                mica_runtime::TaskOutcome::Complete {
                    value,
                    ..
                } if value == Value::bool(true)
            ));
        });
    }
}

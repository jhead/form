//! Port of `packages/ai/src/auth/helpers.ts`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::AuthError;
use crate::interaction::LoginContext;
use crate::provider_auth::{ApiKeyAuth, ApiKeyAuthInput};
use crate::types::{ApiKeyCredential, AuthResult, ModelAuth, TextPrompt};

/// Standard api-key auth: a stored credential key wins, otherwise the first set
/// environment variable resolves. Providers with non-standard resolution
/// (provider env, ambient files, IAM) implement [`ApiKeyAuth`] themselves.
pub struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

impl EnvApiKeyAuth {
    pub fn new(
        name: impl Into<String>,
        env_vars: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            env_vars: env_vars.into_iter().map(Into::into).collect(),
        }
    }

    pub fn shared(
        name: impl Into<String>,
        env_vars: impl IntoIterator<Item = impl Into<String>>,
    ) -> Arc<dyn ApiKeyAuth> {
        Arc::new(Self::new(name, env_vars))
    }
}

#[async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(&self, ctx: &LoginContext) -> Result<ApiKeyCredential, AuthError> {
        ctx.throw_if_aborted()?;
        let key = ctx
            .interaction
            .prompt_secret(TextPrompt::new(format!("Enter {}", self.name)))
            .await?;
        ctx.throw_if_aborted()?;
        Ok(ApiKeyCredential::new(key))
    }

    async fn resolve(&self, input: ApiKeyAuthInput) -> Result<Option<AuthResult>, AuthError> {
        input.throw_if_aborted()?;

        if let Some(credential) = input.credential.as_ref() {
            if let Some(key) = credential.key.as_ref().filter(|k| !k.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth::from_api_key(key.clone()),
                    env: credential.env.clone(),
                    source: Some("stored credential".to_string()),
                }));
            }
        }

        for env_var in &self.env_vars {
            let value = input.ctx.env(env_var).await;
            input.throw_if_aborted()?;
            if let Some(value) = value {
                return Ok(Some(AuthResult {
                    auth: ModelAuth::from_api_key(value),
                    env: Default::default(),
                    source: Some(env_var.clone()),
                }));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::MapAuthContext;

    #[tokio::test]
    async fn a_stored_key_wins_over_the_environment() {
        let auth = EnvApiKeyAuth::new("Anthropic API key", ["ANTHROPIC_API_KEY"]);
        let mut input = ApiKeyAuthInput::new(
            MapAuthContext::new()
                .with_var("ANTHROPIC_API_KEY", "from-env")
                .shared(),
        );
        input.credential = Some(ApiKeyCredential::new("from-store"));

        let result = auth.resolve(input).await.unwrap().expect("resolved");
        assert_eq!(result.auth.api_key.as_deref(), Some("from-store"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
    }

    #[tokio::test]
    async fn the_first_set_environment_variable_resolves() {
        let auth = EnvApiKeyAuth::new("Moonshot API key", ["MOONSHOT_API_KEY", "KIMI_API_KEY"]);
        let input = ApiKeyAuthInput::new(
            MapAuthContext::new()
                .with_var("KIMI_API_KEY", "kimi")
                .shared(),
        );

        let result = auth.resolve(input).await.unwrap().expect("resolved");
        assert_eq!(result.auth.api_key.as_deref(), Some("kimi"));
        assert_eq!(result.source.as_deref(), Some("KIMI_API_KEY"));
    }

    #[tokio::test]
    async fn nothing_configured_resolves_to_none() {
        let auth = EnvApiKeyAuth::new("Anthropic API key", ["ANTHROPIC_API_KEY"]);
        let input = ApiKeyAuthInput::new(MapAuthContext::new().shared());
        assert!(auth.resolve(input).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_credential_without_a_key_still_falls_back_to_the_environment() {
        let auth = EnvApiKeyAuth::new("Anthropic API key", ["ANTHROPIC_API_KEY"]);
        let mut input = ApiKeyAuthInput::new(
            MapAuthContext::new()
                .with_var("ANTHROPIC_API_KEY", "from-env")
                .shared(),
        );
        input.credential = Some(ApiKeyCredential::default());

        let result = auth.resolve(input).await.unwrap().expect("resolved");
        assert_eq!(result.auth.api_key.as_deref(), Some("from-env"));
    }
}

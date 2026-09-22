//! Secret references for tool processes (M4-03.2).
//!
//! A process action never carries an environment *value*. It names a
//! `secret://NAME` reference; policy decides whether the host exposes that
//! reference at all, and the value is read at the last possible moment — after
//! the durable intent exists and immediately before spawn. The value is handed
//! to the child and to the redactor, never to the intent, the receipt, the
//! artifact, or an error message.

use harness_types::{ErrorCode, HarnessError};

use crate::{CodingToolAction, ENV_REFERENCE_PREFIX, EnvBinding};

/// Resolves a `secret://` reference into the value a child process should see.
///
/// Implementations read from wherever the operator keeps the value. The trait
/// returns the value only; every caller that stores or prints tool evidence is
/// responsible for redacting it, and the process runner does exactly that.
pub trait SecretResolver: Send + Sync {
    /// Resolve one reference. The error never contains the value.
    fn resolve(&self, reference: &str) -> Result<String, HarnessError>;
}

/// The host's own environment, read at spawn time.
///
/// This is the default because it is the only source the host already has; a
/// deployment with a keyring or a vault injects its own resolver instead. It is
/// still gated by [`crate::ToolPolicy`]: a reference the operator has not
/// exposed is refused before this trait is consulted.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostEnvironmentSecrets;

impl SecretResolver for HostEnvironmentSecrets {
    fn resolve(&self, reference: &str) -> Result<String, HarnessError> {
        let Some(name) = reference.strip_prefix(ENV_REFERENCE_PREFIX) else {
            return Err(HarnessError::new(
                ErrorCode::EnvironmentDenied,
                "a host secret reference must start with secret://",
            ));
        };
        std::env::var(name).map_err(|_| {
            HarnessError::new(
                ErrorCode::SecretNotGranted,
                format!("secret reference {reference} is not available to this host"),
            )
        })
    }
}

/// The environment one dispatch will hand to a child process.
///
/// Built after the durable intent and used once. `redactions` holds the same
/// values in byte form so the process runner can strip them from stdout and
/// stderr before anything durable sees them.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessEnvironment {
    values: Vec<(String, String)>,
    redactions: Vec<Vec<u8>>,
}

impl ProcessEnvironment {
    /// Nothing beyond the host allowlist.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Resolve every binding of an action through the configured resolver.
    ///
    /// The policy check is repeated here, not trusted from preparation: the
    /// value must not be resolved if the host no longer exposes the reference.
    pub(crate) fn resolve(
        bindings: &[EnvBinding],
        policy_exposes: impl Fn(&str) -> bool,
        resolver: &dyn SecretResolver,
    ) -> Result<Self, HarnessError> {
        let mut environment = Self::default();
        for binding in bindings {
            if !policy_exposes(&binding.reference) {
                return Err(HarnessError::new(
                    ErrorCode::SecretNotGranted,
                    format!(
                        "secret reference {} is not exposed to this host's tool processes",
                        binding.reference
                    ),
                ));
            }
            let value = resolver.resolve(&binding.reference)?;
            if value.is_empty() {
                return Err(HarnessError::new(
                    ErrorCode::SecretNotGranted,
                    format!(
                        "secret reference {} resolved to an empty value",
                        binding.reference
                    ),
                ));
            }
            environment.redactions.push(value.as_bytes().to_vec());
            environment.values.push((binding.name.clone(), value));
        }
        Ok(environment)
    }

    #[must_use]
    pub(crate) fn values(&self) -> &[(String, String)] {
        &self.values
    }

    #[must_use]
    pub(crate) fn redactions(&self) -> &[Vec<u8>] {
        &self.redactions
    }

    /// The environment bindings an action asks for.
    #[must_use]
    pub(crate) fn bindings_of(action: &CodingToolAction) -> &[EnvBinding] {
        match action {
            CodingToolAction::RunProcess { env, .. } | CodingToolAction::RunShell { env, .. } => {
                env
            }
            _ => &[],
        }
    }
}

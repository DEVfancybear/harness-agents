//! The application-created authority context for one operation.
//!
//! A `ScopeContext` is built by the host from transport identity and the
//! effective configuration. Tool arguments and plugin payloads can never widen
//! it: they are checked *against* it, and a target outside the context is
//! refused with `ScopeAuthorityDenied`.
//!
//! Scope filters reads, searches, exports and ranking; matching artifact hashes
//! do not by themselves grant read authority.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ErrorCode, HarnessError, ProducerIdentity, ProjectId, SessionId, TaskId};

/// The identity, ownership and capability envelope of one operation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeContext {
    /// Who is acting. This is provenance, not a trusted service lookup.
    pub principal: ProducerIdentity,
    pub project_id: ProjectId,
    /// Workspace/worktree the operation may touch.
    pub worktree_id: String,
    pub task_id: TaskId,
    pub session_id: SessionId,
    /// Capabilities the host grants for this operation.
    pub capabilities: BTreeSet<String>,
    /// The configuration revision this scope was built from.
    pub config_revision: u64,
    /// Ownership generation the host holds.
    pub owner_generation: u64,
}

impl ScopeContext {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.principal.validate()?;
        if self.worktree_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "scope.worktree_id must not be empty",
            ));
        }
        if self.config_revision == 0 {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "scope.config_revision must start at 1",
            ));
        }
        if self
            .capabilities
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "scope capabilities must not be empty strings",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    /// True when every field the target names matches this context. A field the
    /// target leaves out is not constrained by this check.
    #[must_use]
    pub fn authorizes(&self, target: &ScopeTarget) -> bool {
        if target
            .project_id
            .as_ref()
            .is_some_and(|project| project != &self.project_id)
        {
            return false;
        }
        if target
            .task_id
            .as_ref()
            .is_some_and(|task| task != &self.task_id)
        {
            return false;
        }
        if target
            .session_id
            .as_ref()
            .is_some_and(|session| session != &self.session_id)
        {
            return false;
        }
        if target
            .worktree_id
            .as_ref()
            .is_some_and(|worktree| worktree != &self.worktree_id)
        {
            return false;
        }
        true
    }

    pub fn require(&self, target: &ScopeTarget) -> Result<(), HarnessError> {
        if self.authorizes(target) {
            Ok(())
        } else {
            Err(HarnessError::new(
                ErrorCode::ScopeAuthorityDenied,
                "the requested target is outside the operation scope",
            ))
        }
    }
}

/// The fields an operation claims to touch. `None` means the operation does not
/// claim a value for that field, so the check cannot be widened by omission of
/// the field it means to use.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScopeTarget {
    pub project_id: Option<ProjectId>,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<String>,
}

impl ScopeTarget {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn project(mut self, project_id: ProjectId) -> Self {
        self.project_id = Some(project_id);
        self
    }

    #[must_use]
    pub fn task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    #[must_use]
    pub fn session(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    #[must_use]
    pub fn worktree(mut self, worktree_id: impl Into<String>) -> Self {
        self.worktree_id = Some(worktree_id.into());
        self
    }

    #[must_use]
    pub fn is_unconstrained(&self) -> bool {
        self.project_id.is_none()
            && self.task_id.is_none()
            && self.session_id.is_none()
            && self.worktree_id.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::{ScopeContext, ScopeTarget};
    use crate::{ErrorCode, ProducerIdentity, ProjectId, SessionId, TaskId};

    fn context() -> ScopeContext {
        ScopeContext {
            principal: ProducerIdentity {
                plugin_id: "m0.test".to_owned(),
                implementation_version: "1".to_owned(),
            },
            project_id: ProjectId::generate(),
            worktree_id: "worktree-a".to_owned(),
            task_id: TaskId::generate(),
            session_id: SessionId::generate(),
            capabilities: ["tools.read".to_owned(), "tools.write".to_owned()]
                .into_iter()
                .collect(),
            config_revision: 1,
            owner_generation: 0,
        }
    }

    #[test]
    fn an_empty_target_is_authorized_and_a_foreign_target_is_not() {
        let scope = context();
        assert!(scope.authorizes(&ScopeTarget::new()));
        assert!(
            scope.authorizes(
                &ScopeTarget::new()
                    .project(scope.project_id.clone())
                    .task(scope.task_id.clone())
                    .session(scope.session_id.clone())
                    .worktree("worktree-a")
            )
        );
        assert!(!scope.authorizes(&ScopeTarget::new().project(ProjectId::generate())));
        assert!(!scope.authorizes(&ScopeTarget::new().task(TaskId::generate())));
        assert!(!scope.authorizes(&ScopeTarget::new().session(SessionId::generate())));
        assert!(!scope.authorizes(&ScopeTarget::new().worktree("worktree-b")));
    }

    #[test]
    fn require_reports_the_typed_authority_code() {
        let scope = context();
        let error = scope
            .require(&ScopeTarget::new().task(TaskId::generate()))
            .expect_err("a foreign task must be refused");
        assert_eq!(error.code(), ErrorCode::ScopeAuthorityDenied);
    }

    #[test]
    fn capabilities_and_validation_are_explicit() {
        let scope = context();
        assert!(scope.has_capability("tools.read"));
        assert!(!scope.has_capability("tools.network"));
        scope.validate().expect("a built scope is valid");

        let mut empty_worktree = scope.clone();
        empty_worktree.worktree_id = "  ".to_owned();
        assert_eq!(
            empty_worktree
                .validate()
                .expect_err("blank worktree is invalid")
                .code(),
            ErrorCode::InvalidPayload
        );

        let mut zero_revision = scope;
        zero_revision.config_revision = 0;
        assert_eq!(
            zero_revision
                .validate()
                .expect_err("revision must start at 1")
                .code(),
            ErrorCode::InvalidPayload
        );
    }
}

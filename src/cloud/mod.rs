use crate::error::Error;
use crate::schema::{RepoId, TagSpec};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceState {
    Pending,
    Running,
    ShuttingDown,
    Terminated,
    Stopping,
    Stopped,
}

impl InstanceState {
    pub fn is_live(self) -> bool {
        match self {
            InstanceState::Pending
            | InstanceState::Running
            | InstanceState::Stopping
            | InstanceState::Stopped => true,
            InstanceState::ShuttingDown | InstanceState::Terminated => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_instance_is_live() {
        // A stopped instance still exists and bills EBS: it must be adopted
        // and swept, never silently dropped.
        assert!(InstanceState::Stopped.is_live());
    }
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub state: InstanceState,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub count: u32,
    pub image_id: String,
    pub instance_type: String,
    pub spot: bool,
    pub tags: TagSpec,
    pub user_data: String,
}

/// The only seam to a cloud. Sync in phase 1; phase 2 makes these async when
/// aws-sdk-rust arrives.
pub trait Cloud {
    /// Launch spec.count instances with spec.tags applied atomically at creation.
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error>;
    fn terminate(&mut self, ids: &[String]) -> Result<(), Error>;
    /// All non-terminated instances carrying burst-actions=1 and burst-actions-repo=<repo>.
    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error>;
    /// Arm the control-plane one-shot kill for one instance at `at`.
    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error>;
    /// Get-or-create the image for `key`; returns the image id.
    fn bake(&mut self, key: &str) -> Result<String, Error>;
}

pub mod fake;

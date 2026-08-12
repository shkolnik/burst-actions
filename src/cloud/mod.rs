use crate::error::Error;
use crate::schema::{RepoId, TagSpec, VolumeSpec};
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
    /// EC2 key-pair name for SSH access. `None` (the default) launches with
    /// no SSH key.
    pub ssh_key: Option<String>,
    /// Root volume for every instance in this launch.
    pub volume: VolumeSpec,
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
    /// Delete the one-shot kill schedule for `instance_id`. Already-gone
    /// (fired and self-deleted, or never armed) is Ok — disarming is
    /// idempotent.
    fn disarm_kill(&mut self, instance_id: &str) -> Result<(), Error>;
    /// Instance ids that currently have an armed kill schedule
    /// (burst-actions-<id>), across all repos.
    fn list_armed_kills(&self) -> Result<Vec<String>, Error>;
    /// All non-terminated instances carrying burst-actions=1, ANY repo.
    /// The sweep's orphan-schedule check must see other repos' live
    /// instances, or it would mistake their schedules for orphans.
    fn list_all_tagged(&self) -> Result<Vec<Instance>, Error>;
}

pub mod aws;
pub mod fake;

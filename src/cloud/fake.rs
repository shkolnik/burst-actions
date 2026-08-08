use super::{Cloud, Instance, InstanceState, LaunchSpec};
use crate::error::Error;
use crate::schema::{RepoId, TAG_BURST, TAG_REPO};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct FakeCloud {
    instances: Vec<Instance>,
    kills: Vec<(String, DateTime<Utc>)>,
    images: BTreeMap<String, String>,
    next_id: u32,
}

impl FakeCloud {
    pub fn set_state(&mut self, id: &str, s: InstanceState) {
        let i = self
            .instances
            .iter_mut()
            .find(|i| i.id == id)
            .unwrap_or_else(|| panic!("no such fake instance {id}"));
        i.state = s;
    }

    pub fn armed_kills(&self) -> &[(String, DateTime<Utc>)] {
        &self.kills
    }

    pub fn plant(&mut self, instance: Instance) {
        self.instances.push(instance);
    }
}

impl Cloud for FakeCloud {
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error> {
        let mut out = Vec::new();
        for _ in 0..spec.count {
            let instance = Instance {
                id: format!("i-fake-{}", self.next_id),
                state: InstanceState::Running,
                tags: spec.tags.to_tags().into_iter().collect(),
            };
            self.next_id += 1;
            self.instances.push(instance.clone());
            out.push(instance);
        }
        Ok(out)
    }

    fn terminate(&mut self, ids: &[String]) -> Result<(), Error> {
        for i in &mut self.instances {
            if ids.contains(&i.id) {
                i.state = InstanceState::Terminated;
            }
        }
        Ok(())
    }

    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error> {
        let repo = repo.to_string();
        Ok(self
            .instances
            .iter()
            .filter(|i| i.state != InstanceState::Terminated)
            .filter(|i| i.tags.iter().any(|(k, v)| k == TAG_REPO && *v == repo))
            .filter(|i| i.tags.iter().any(|(k, v)| k == TAG_BURST && v == "1"))
            .cloned()
            .collect())
    }

    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error> {
        self.kills.push((instance_id.to_string(), at));
        Ok(())
    }

    fn bake(&mut self, key: &str) -> Result<String, Error> {
        Ok(self
            .images
            .entry(key.to_string())
            .or_insert_with(|| format!("ami-fake-{key}"))
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RepoId, TAG_REPO, TagSpec};
    use chrono::{Duration, Utc};

    fn spec(repo: &str, count: u32) -> LaunchSpec {
        LaunchSpec {
            count,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec {
                repo: RepoId::parse(repo).unwrap(),
                expires: Utc::now() + Duration::hours(6),
            },
            user_data: "jit-blob".into(),
        }
    }

    #[test]
    fn launch_applies_tags_atomically_and_lists_by_repo() {
        let mut c = FakeCloud::default();
        let launched = c.launch(&spec("octo/widgets", 2)).unwrap();
        assert_eq!(launched.len(), 2);
        for i in &launched {
            assert!(
                i.tags
                    .iter()
                    .any(|(k, v)| k == TAG_REPO && v == "octo/widgets")
            );
        }
        c.launch(&spec("other/repo", 1)).unwrap();
        let listed = c
            .list_tagged(&RepoId::parse("octo/widgets").unwrap())
            .unwrap();
        assert_eq!(listed.len(), 2, "must filter by burst-repo");
    }

    #[test]
    fn terminated_instances_leave_the_listing() {
        let mut c = FakeCloud::default();
        let ids: Vec<String> = c
            .launch(&spec("octo/widgets", 2))
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        c.terminate(&ids[..1]).unwrap();
        let listed = c
            .list_tagged(&RepoId::parse("octo/widgets").unwrap())
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, ids[1]);
    }

    #[test]
    fn arm_kill_records_per_instance_schedules() {
        let mut c = FakeCloud::default();
        let at = Utc::now() + Duration::hours(6);
        let launched = c.launch(&spec("octo/widgets", 1)).unwrap();
        c.arm_kill(&launched[0].id, at).unwrap();
        assert_eq!(c.armed_kills(), &[(launched[0].id.clone(), at)]);
    }

    #[test]
    fn list_tagged_excludes_instances_missing_burst_tag() {
        let mut c = FakeCloud::default();
        c.plant(Instance {
            id: "i-untagged".into(),
            state: InstanceState::Running,
            tags: vec![(TAG_REPO.into(), "octo/widgets".into())],
        });
        let listed = c
            .list_tagged(&RepoId::parse("octo/widgets").unwrap())
            .unwrap();
        assert!(
            listed.is_empty(),
            "instance without burst=1 must not be listed: {listed:?}"
        );
    }

    #[test]
    fn bake_is_get_or_create_per_key() {
        let mut c = FakeCloud::default();
        let a = c.bake("v1-abc").unwrap();
        assert_eq!(a, c.bake("v1-abc").unwrap());
        assert_ne!(a, c.bake("v1-def").unwrap());
    }
}

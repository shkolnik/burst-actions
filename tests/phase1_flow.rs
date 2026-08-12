use burst::cloud::fake::FakeCloud;
use burst::cloud::{Cloud, InstanceState, LaunchSpec};
use burst::reconcile::reconcile;
use burst::schema::{RepoId, TagSpec};
use burst::state::{InstanceRecord, RepoState, StateFile};
use chrono::{Duration, Utc};

#[test]
fn abandoned_run_is_adopted_and_reconciled() {
    let repo = RepoId::parse("octo/widgets").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut cloud = FakeCloud::default();

    // Invocation 1: launch 3, record them, then "crash" (drop the lock, keep state).
    let launched = cloud
        .launch(&LaunchSpec {
            count: 3,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec {
                repo: repo.clone(),
                expires: Utc::now() + Duration::hours(6),
            },
            user_data: "jit".into(),
            ssh_key: None,
            volume: burst::schema::VolumeSpec::default(),
        })
        .unwrap();
    let rs = RepoState::open_at(dir.path().to_path_buf());
    {
        let _lock = rs.lock().unwrap();
        rs.write(&StateFile {
            version: burst::state::STATE_VERSION,
            repo: repo.to_string(),
            instances: launched
                .iter()
                .map(|i| InstanceRecord {
                    id: i.id.clone(),
                    runner: None,
                    launched_at: Utc::now(),
                    expires_at: Utc::now() + Duration::hours(6),
                })
                .collect(),
        })
        .unwrap();
    } // lock evaporates; statefile remains — the abandoned-run signal

    // Meanwhile: one VM finishes its job (self-terminates), and another host's
    // instance for the same repo appears.
    cloud.set_state(&launched[0].id, InstanceState::Terminated);
    let stranger = cloud
        .launch(&LaunchSpec {
            count: 1,
            image_id: "ami-fake-k".into(),
            instance_type: "t3.micro".into(),
            spot: false,
            tags: TagSpec {
                repo: repo.clone(),
                expires: Utc::now() + Duration::hours(6),
            },
            user_data: "jit2".into(),
            ssh_key: None,
            volume: burst::schema::VolumeSpec::default(),
        })
        .unwrap();

    // Invocation 2: lock is acquirable + statefile present → adopt.
    let _lock = rs.lock().unwrap();
    let state = rs.read().unwrap().expect("abandoned statefile present");
    let live_cloud = cloud.list_tagged(&repo).unwrap();
    let r = reconcile(&state, &live_cloud);
    assert_eq!(r.dropped, vec![launched[0].id.clone()]);
    assert_eq!(r.adopted, vec![stranger[0].id.clone()]);
    assert_eq!(r.live.len(), 3); // 2 survivors + 1 adopted

    // The reconciled manifest is written back atomically.
    rs.write(&StateFile {
        version: burst::state::STATE_VERSION,
        repo: repo.to_string(),
        instances: r.live,
    })
    .unwrap();
    assert_eq!(rs.read().unwrap().unwrap().instances.len(), 3);
}

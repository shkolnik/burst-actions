//! Gate diagnostic: launch one tagged, kill-armed t3.micro with an arbitrary
//! user-data script and poll until termination (poweroff => terminate) or a
//! cap. The only observable from a VM without console-output permission is
//! WHEN it dies — this bisects the provisioning hang. Removed after the gate.

use burst::cloud::Cloud;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: probe_userdata <config-dir> <script-file> <cap-min>");
    let script_file = args.next().expect("script file");
    let cap_min: i64 = args.next().expect("cap minutes").parse().unwrap();
    let raw = std::fs::read_to_string(&script_file).expect("read script");
    let image_override = args.next();
    let user_data = if script_file.ends_with(".blob") {
        burst::payload::fleet_user_data(raw.trim_end()).expect("valid jit blob")
    } else {
        raw
    };

    let config = burst::config::load(std::path::Path::new(&dir), None).expect("config");
    let ctx = burst::cloud::aws::AwsContext::connect(config.region.as_deref()).expect("connect");
    let substrate = ctx
        .ensure_substrate(config.budget_alarm_usd)
        .expect("substrate");
    let expires = chrono::Utc::now() + chrono::Duration::minutes(cap_min);
    let mut cloud = burst::cloud::aws::AwsCloud {
        ctx,
        substrate,
        repo: config.repo.clone(),
        base_ami: config.base_ami.clone().expect("base_ami"),
        builder_instance_type: config.instance_type.clone(),
        provisioning_script: String::new(),
    };
    let spec = burst::cloud::LaunchSpec {
        count: 1,
        image_id: image_override.unwrap_or_else(|| config.base_ami.clone().expect("base_ami")),
        instance_type: config.instance_type.clone(),
        spot: false,
        tags: burst::schema::TagSpec {
            repo: config.repo.clone(),
            expires,
        },
        user_data,
    };
    let launched = cloud.launch(&spec).expect("launch");
    let id = launched[0].id.clone();
    let t0 = std::time::Instant::now();
    println!("launched {id} at {}", chrono::Utc::now().to_rfc3339());
    cloud.arm_kill(&id, expires).expect("arm_kill");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let fleet = cloud.list_tagged(&config.repo).expect("list");
        let state = fleet
            .iter()
            .find(|i| i.id == id)
            .map(|i| format!("{:?}", i.state))
            .unwrap_or_else(|| "gone-from-listing (terminated)".into());
        println!("t+{:>4}s  {state}", t0.elapsed().as_secs());
        if state.contains("erminated") || state.contains("gone") {
            println!("terminal after {:?}", t0.elapsed());
            break;
        }
        if t0.elapsed().as_secs() > (cap_min as u64 + 3) * 60 {
            println!("cap exceeded; kill schedule owns it");
            break;
        }
    }
}

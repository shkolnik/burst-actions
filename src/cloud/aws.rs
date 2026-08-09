use super::{Cloud, Instance, InstanceState, LaunchSpec};
use crate::error::Error;
use crate::schema::{Arch, RepoId, TAG_BURST, TAG_IMAGE_KEY, TAG_REPO};
use aws_sdk_ec2::error::ProvideErrorMetadata;
use aws_smithy_types::error::display::DisplayErrorContext;
use base64::Engine;
use chrono::{DateTime, Utc};
use std::time::Duration;

/// Substrings that, when found in the full error chain (or an AWS service
/// error code), indicate the failure is a credentials/auth problem rather
/// than some other API error — e.g. a `DescribeVpcs` dispatch failure with
/// no credentials in the provider chain, or a service error like
/// `InvalidClientTokenId` for a bad/expired key. Kept conservative: matching
/// text appends the remedy, it never suppresses the underlying message.
const CREDENTIAL_ERROR_MARKERS: &[&str] = &[
    "credentials",
    "InvalidClientTokenId",
    "AuthFailure",
    "UnrecognizedClientException",
    "SignatureDoesNotMatch",
    "ExpiredToken",
];

/// True if the full error text names a credentials/auth failure.
fn is_credentials_error(full_message: &str) -> bool {
    let lower = full_message.to_lowercase();
    CREDENTIAL_ERROR_MARKERS
        .iter()
        .any(|marker| lower.contains(&marker.to_lowercase()))
}

/// Format an AWS SDK error for `Error::Aws { message, .. }`: walk the full
/// `source()` chain (bare `Display`/`to_string()` on `SdkError` hides the
/// underlying cause, e.g. `InvalidClientTokenId`) and, if the chain looks
/// like a credentials/auth failure, append the specified remedy text. Single
/// authoring site so every AWS call reports errors the same way.
fn format_aws_error<E: std::error::Error + 'static>(err: &E) -> String {
    let full = format!("{}", DisplayErrorContext(err));
    if is_credentials_error(&full) {
        format!("{full} (configure AWS credentials: env vars or `aws configure`)")
    } else {
        full
    }
}

/// Live AWS context: a single-thread runtime plus the clients this crate needs.
///
/// Construction (`connect`) and the VPC probe both make network calls, so
/// nothing in this module runs at `cargo test` time unless explicitly
/// invoked; only [`effective_region`] is unit-tested.
pub struct AwsContext {
    runtime: tokio::runtime::Runtime,
    ec2: aws_sdk_ec2::Client,
    scheduler: aws_sdk_scheduler::Client,
    iam: aws_sdk_iam::Client,
    budgets: aws_sdk_budgets::Client,
    sts: aws_sdk_sts::Client,
}

/// The substrate `ensure_substrate` produces: the roles, security group, and
/// subnet a launch needs. Every field is get-or-created, never assumed.
#[derive(Debug, Clone)]
pub struct Substrate {
    pub instance_profile_name: String,
    pub scheduler_role_arn: String,
    pub security_group_id: String,
    pub subnet_id: String,
}

const INSTANCE_ROLE_NAME: &str = "burst-actions-instance";
const SCHEDULER_ROLE_NAME: &str = "burst-actions-scheduler";
const SCHEDULER_POLICY_NAME: &str = "burst-actions-terminate";
const SECURITY_GROUP_NAME: &str = "burst-actions";
const BUDGET_NAME: &str = "burst-actions-monthly";

/// IAM trust ("assume role") policy for `service` — the only principal
/// allowed to assume the role.
fn trust_policy(service: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Inline policy for `burst-actions-scheduler`: it may terminate EC2
/// instances, but only ones tagged `burst-actions=1` — the tag-fenced kill
/// invariant 4 relies on.
fn scheduler_kill_policy() -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "ec2:TerminateInstances",
            "Resource": "arn:aws:ec2:*:*:instance/*",
            "Condition": {
                "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
            }
        }]
    })
    .to_string()
}

/// Resolve the region to use: config (`burst.toml`) wins over the resolved
/// provider chain (env var / profile). Neither present is fail-loud.
fn effective_region(
    config_region: Option<&str>,
    chain_region: Option<&str>,
) -> Result<String, Error> {
    config_region
        .or(chain_region)
        .map(str::to_string)
        .ok_or(Error::RegionMissing)
}

impl AwsContext {
    /// Build a single-thread runtime, resolve the region (config wins over
    /// the env/profile chain), load `SdkConfig`, and construct the clients.
    pub fn connect(region_override: Option<&str>) -> Result<AwsContext, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| Error::Aws {
                op: "build tokio runtime",
                message: source.to_string(),
            })?;

        let sdk_config = runtime.block_on(async {
            // Resolve the chain's region first so config can override it
            // without suppressing the rest of the chain (credentials, etc).
            let probe = aws_config::defaults(aws_config::BehaviorVersion::latest())
                .load()
                .await;
            let chain_region = probe.region().map(|r| r.as_ref().to_string());
            let region = effective_region(region_override, chain_region.as_deref())?;

            Ok::<_, Error>(
                aws_config::defaults(aws_config::BehaviorVersion::latest())
                    .region(aws_sdk_ec2::config::Region::new(region))
                    .load()
                    .await,
            )
        })?;

        let ec2 = aws_sdk_ec2::Client::new(&sdk_config);
        let scheduler = aws_sdk_scheduler::Client::new(&sdk_config);
        let iam = aws_sdk_iam::Client::new(&sdk_config);
        let budgets = aws_sdk_budgets::Client::new(&sdk_config);
        let sts = aws_sdk_sts::Client::new(&sdk_config);

        Ok(AwsContext {
            runtime,
            ec2,
            scheduler,
            iam,
            budgets,
            sts,
        })
    }

    /// The default VPC and its default-for-AZ subnet (first by AZ name, for
    /// determinism). Fails loud if there is no default VPC.
    pub fn default_vpc_and_subnet(&self) -> Result<(String, String), Error> {
        self.runtime.block_on(async {
            let vpcs = self
                .ec2
                .describe_vpcs()
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("is-default")
                        .values("true")
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeVpcs",
                    message: format_aws_error(&e),
                })?;

            let region = self
                .ec2
                .config()
                .region()
                .map(|r| r.as_ref().to_string())
                .unwrap_or_default();

            let vpc_id = vpcs
                .vpcs()
                .first()
                .and_then(|v| v.vpc_id())
                .ok_or_else(|| Error::NoDefaultVpc {
                    region: region.clone(),
                })?
                .to_string();

            let subnets = self
                .ec2
                .describe_subnets()
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("vpc-id")
                        .values(&vpc_id)
                        .build(),
                )
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("default-for-az")
                        .values("true")
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeSubnets",
                    message: format_aws_error(&e),
                })?;

            let mut by_az: Vec<_> = subnets
                .subnets()
                .iter()
                .filter_map(|s| Some((s.availability_zone()?, s.subnet_id()?)))
                .collect();
            by_az.sort_by_key(|(az, _)| az.to_string());

            let subnet_id = by_az
                .first()
                .map(|(_, id)| id.to_string())
                .ok_or(Error::NoDefaultVpc { region })?;

            Ok((vpc_id, subnet_id))
        })
    }

    /// Idempotent get-or-create of every substrate resource: the instance
    /// role/profile, the tag-fenced scheduler role, the zero-inbound
    /// security group, and (opt-in) a monthly budget alarm. Safe to call on
    /// a fresh account or the thousandth run — same code path either way.
    pub fn ensure_substrate(&self, budget_alarm_usd: Option<u32>) -> Result<Substrate, Error> {
        let (_vpc_id, subnet_id) = self.default_vpc_and_subnet()?;

        self.runtime.block_on(async {
            let instance_profile_name = self.ensure_instance_role_and_profile().await?;
            let scheduler_role_arn = self.ensure_scheduler_role().await?;
            let security_group_id = self.ensure_security_group(&_vpc_id).await?;

            if let Some(limit_usd) = budget_alarm_usd {
                self.ensure_budget(limit_usd).await?;
            }

            Ok(Substrate {
                instance_profile_name,
                scheduler_role_arn,
                security_group_id,
                subnet_id,
            })
        })
    }

    /// Role `burst-actions-instance` (trust ec2.amazonaws.com, no policies —
    /// invariant 4's near-empty profile) + matching instance profile, with
    /// the role attached. Returns the instance profile name.
    async fn ensure_instance_role_and_profile(&self) -> Result<String, Error> {
        match self
            .iam
            .get_role()
            .role_name(INSTANCE_ROLE_NAME)
            .send()
            .await
        {
            Ok(_) => {}
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_no_such_entity_exception()) =>
            {
                self.iam
                    .create_role()
                    .role_name(INSTANCE_ROLE_NAME)
                    .assume_role_policy_document(trust_policy("ec2.amazonaws.com"))
                    .send()
                    .await
                    .map_err(|e| Error::Aws {
                        op: "CreateRole(burst-actions-instance)",
                        message: format_aws_error(&e),
                    })?;
            }
            Err(e) => {
                return Err(Error::Aws {
                    op: "GetRole(burst-actions-instance)",
                    message: format_aws_error(&e),
                });
            }
        }

        match self
            .iam
            .get_instance_profile()
            .instance_profile_name(INSTANCE_ROLE_NAME)
            .send()
            .await
        {
            Ok(_) => {}
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_no_such_entity_exception()) =>
            {
                self.iam
                    .create_instance_profile()
                    .instance_profile_name(INSTANCE_ROLE_NAME)
                    .send()
                    .await
                    .map_err(|e| Error::Aws {
                        op: "CreateInstanceProfile(burst-actions-instance)",
                        message: format_aws_error(&e),
                    })?;
            }
            Err(e) => {
                return Err(Error::Aws {
                    op: "GetInstanceProfile(burst-actions-instance)",
                    message: format_aws_error(&e),
                });
            }
        }

        match self
            .iam
            .add_role_to_instance_profile()
            .instance_profile_name(INSTANCE_ROLE_NAME)
            .role_name(INSTANCE_ROLE_NAME)
            .send()
            .await
        {
            Ok(_) => {}
            // LimitExceeded: an instance profile holds at most one role, and
            // ours is already attached — already-done, not a failure.
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_limit_exceeded_exception()) => {}
            Err(e) => {
                return Err(Error::Aws {
                    op: "AddRoleToInstanceProfile(burst-actions-instance)",
                    message: format_aws_error(&e),
                });
            }
        }

        Ok(INSTANCE_ROLE_NAME.to_string())
    }

    /// Role `burst-actions-scheduler` (trust scheduler.amazonaws.com) with
    /// the inline `burst-actions-terminate` policy scoping it to killing
    /// only tagged instances. Returns the role ARN.
    async fn ensure_scheduler_role(&self) -> Result<String, Error> {
        let arn = match self
            .iam
            .get_role()
            .role_name(SCHEDULER_ROLE_NAME)
            .send()
            .await
        {
            Ok(out) => out
                .role()
                .map(|r| r.arn().to_string())
                .ok_or_else(|| Error::Aws {
                    op: "GetRole(burst-actions-scheduler)",
                    message: "response had no role".to_string(),
                })?,
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_no_such_entity_exception()) =>
            {
                let out = self
                    .iam
                    .create_role()
                    .role_name(SCHEDULER_ROLE_NAME)
                    .assume_role_policy_document(trust_policy("scheduler.amazonaws.com"))
                    .send()
                    .await
                    .map_err(|e| Error::Aws {
                        op: "CreateRole(burst-actions-scheduler)",
                        message: format_aws_error(&e),
                    })?;
                out.role()
                    .map(|r| r.arn().to_string())
                    .ok_or_else(|| Error::Aws {
                        op: "CreateRole(burst-actions-scheduler)",
                        message: "response had no role".to_string(),
                    })?
            }
            Err(e) => {
                return Err(Error::Aws {
                    op: "GetRole(burst-actions-scheduler)",
                    message: format_aws_error(&e),
                });
            }
        };

        // PutRolePolicy overwrites unconditionally on the given name, so
        // this is idempotent by construction — no need to check existence.
        self.iam
            .put_role_policy()
            .role_name(SCHEDULER_ROLE_NAME)
            .policy_name(SCHEDULER_POLICY_NAME)
            .policy_document(scheduler_kill_policy())
            .send()
            .await
            .map_err(|e| Error::Aws {
                op: "PutRolePolicy(burst-actions-terminate)",
                message: format_aws_error(&e),
            })?;

        Ok(arn)
    }

    /// Security group `burst-actions` in `vpc_id`: get-or-create, zero
    /// ingress rules ever added (a fresh SG starts zero-inbound; we hold no
    /// rule-editing permission, so the absence is IAM-enforced, not just
    /// convention).
    async fn ensure_security_group(&self, vpc_id: &str) -> Result<String, Error> {
        let existing = self
            .ec2
            .describe_security_groups()
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("group-name")
                    .values(SECURITY_GROUP_NAME)
                    .build(),
            )
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("vpc-id")
                    .values(vpc_id)
                    .build(),
            )
            .send()
            .await
            .map_err(|e| Error::Aws {
                op: "DescribeSecurityGroups",
                message: format_aws_error(&e),
            })?;

        if let Some(id) = existing
            .security_groups()
            .first()
            .and_then(|g| g.group_id())
        {
            return Ok(id.to_string());
        }

        let created = self
            .ec2
            .create_security_group()
            .group_name(SECURITY_GROUP_NAME)
            .description("burst-actions fleet instances (zero inbound)")
            .vpc_id(vpc_id)
            .tag_specifications(
                aws_sdk_ec2::types::TagSpecification::builder()
                    .resource_type(aws_sdk_ec2::types::ResourceType::SecurityGroup)
                    .tags(
                        aws_sdk_ec2::types::Tag::builder()
                            .key("burst-actions")
                            .value("1")
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await
            .map_err(|e| Error::Aws {
                op: "CreateSecurityGroup(burst-actions)",
                message: format_aws_error(&e),
            })?;

        created
            .group_id()
            .map(str::to_string)
            .ok_or_else(|| Error::Aws {
                op: "CreateSecurityGroup(burst-actions)",
                message: "response had no group id".to_string(),
            })
    }

    /// Opt-in monthly cost budget `burst-actions-monthly` with limit
    /// `limit_usd`. Get-or-create on the `budgets` global (us-east-1)
    /// endpoint. Only called when the caller opted in.
    async fn ensure_budget(&self, limit_usd: u32) -> Result<(), Error> {
        let account_id = self
            .sts
            .get_caller_identity()
            .send()
            .await
            .map_err(|e| Error::Aws {
                op: "GetCallerIdentity",
                message: format_aws_error(&e),
            })?
            .account()
            .ok_or_else(|| Error::Aws {
                op: "GetCallerIdentity",
                message: "response had no account id".to_string(),
            })?
            .to_string();

        match self
            .budgets
            .describe_budget()
            .account_id(&account_id)
            .budget_name(BUDGET_NAME)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|s| s.is_not_found_exception()) =>
            {
                let budget = aws_sdk_budgets::types::Budget::builder()
                    .budget_name(BUDGET_NAME)
                    .budget_limit(
                        aws_sdk_budgets::types::Spend::builder()
                            .amount(limit_usd.to_string())
                            .unit("USD")
                            .build()
                            .map_err(|e| Error::Aws {
                                op: "CreateBudget(burst-actions-monthly)",
                                message: format_aws_error(&e),
                            })?,
                    )
                    .time_unit(aws_sdk_budgets::types::TimeUnit::Monthly)
                    .budget_type(aws_sdk_budgets::types::BudgetType::Cost)
                    .build()
                    .map_err(|e| Error::Aws {
                        op: "CreateBudget(burst-actions-monthly)",
                        message: format_aws_error(&e),
                    })?;

                self.budgets
                    .create_budget()
                    .account_id(&account_id)
                    .budget(budget)
                    .send()
                    .await
                    .map_err(|e| Error::Aws {
                        op: "CreateBudget(burst-actions-monthly)",
                        message: format_aws_error(&e),
                    })?;
                Ok(())
            }
            Err(e) => Err(Error::Aws {
                op: "DescribeBudget(burst-actions-monthly)",
                message: format_aws_error(&e),
            }),
        }
    }

    /// The region this context is connected to (for error messages only —
    /// `effective_region` is the source of truth at connect time).
    pub fn region_str(&self) -> String {
        self.ec2
            .config()
            .region()
            .map(|r| r.as_ref().to_string())
            .unwrap_or_default()
    }

    /// Resolve the current Debian 13 (trixie) AMI id for `arch`: official
    /// Debian owner account (136693071363), newest by creation date.
    /// Read-only `DescribeImages` — never creates or pins anything; §8.6
    /// requires the caller to pin the result into `burst.toml` explicitly.
    /// Ubuntu (or anything else) stays supported via an explicit
    /// `base_ami` pin.
    pub fn resolve_latest_debian_ami(&self, arch: Arch) -> Result<String, Error> {
        let arch_name = match arch {
            Arch::X86_64 => "amd64",
            Arch::Arm64 => "arm64",
        };
        let name_filter = format!("debian-13-{arch_name}-*");

        self.runtime.block_on(async {
            let out = self
                .ec2
                .describe_images()
                .owners("136693071363")
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("name")
                        .values(&name_filter)
                        .build(),
                )
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("state")
                        .values("available")
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeImages(debian)",
                    message: format_aws_error(&e),
                })?;

            let mut images: Vec<(&str, &str)> = out
                .images()
                .iter()
                .filter_map(|i| Some((i.image_id()?, i.creation_date()?)))
                .collect();
            // Newest first: creation_date is ISO8601, so lexicographic order
            // is chronological order.
            images.sort_by(|a, b| b.1.cmp(a.1));

            images
                .first()
                .map(|(id, _)| id.to_string())
                .ok_or_else(|| Error::Aws {
                    op: "DescribeImages(debian)",
                    message: format!(
                        "no available Debian 13 AMI found for {arch_name} in this region"
                    ),
                })
        })
    }
}

/// True for the transient invalid-instance-profile error RunInstances returns
/// in the seconds after ensure_substrate() creates the role on a fresh
/// account (AWS IAM is eventually consistent). Verified live at the phase-2
/// gate; the message fragment is AWS's, not ours.
pub(crate) fn is_iam_propagation_error(code: &str, message: &str) -> bool {
    code == "InvalidParameterValue" && message.contains("Invalid IAM Instance Profile")
}

/// Schedule name for an instance's one-shot kill: burst-actions-<instance-id>.
pub(crate) fn kill_schedule_name(instance_id: &str) -> String {
    format!("burst-actions-{instance_id}")
}

/// EventBridge Scheduler one-shot expression: at(yyyy-mm-ddThh:mm:ss), UTC, no zone suffix.
pub(crate) fn at_expression(at: DateTime<Utc>) -> String {
    format!("at({})", at.format("%Y-%m-%dT%H:%M:%S"))
}

/// Bounded backoff for the IAM-propagation retry: 6 delays summing to ~60s.
pub(crate) fn retry_delays() -> impl Iterator<Item = Duration> {
    [2u64, 4, 8, 15, 15, 15]
        .into_iter()
        .map(Duration::from_secs)
}

/// Map an EC2 `instance-state-name` into our closed [`InstanceState`] set.
/// Exhaustive over the six documented names; an unrecognized name is a fail
/// -loud `Error::Aws`, never a silent skip.
#[allow(clippy::wildcard_enum_match_arm)] // N is #[non_exhaustive]; the wildcard is the fail-loud catch-all for unrecognized/future state names, not a lazy default
fn map_instance_state(
    name: &aws_sdk_ec2::types::InstanceStateName,
) -> Result<InstanceState, Error> {
    use aws_sdk_ec2::types::InstanceStateName as N;
    match name {
        N::Pending => Ok(InstanceState::Pending),
        N::Running => Ok(InstanceState::Running),
        N::ShuttingDown => Ok(InstanceState::ShuttingDown),
        N::Terminated => Ok(InstanceState::Terminated),
        N::Stopping => Ok(InstanceState::Stopping),
        N::Stopped => Ok(InstanceState::Stopped),
        other => Err(Error::Aws {
            op: "DescribeInstances",
            message: format!("unrecognized instance state name {other:?}"),
        }),
    }
}

/// Live AWS backend: the `Cloud` seam over `AwsContext` + the substrate
/// `ensure_substrate` produced.
pub struct AwsCloud {
    pub ctx: AwsContext,
    pub substrate: Substrate,
    /// Target repository — not part of the `Cloud::bake` signature (that
    /// trait method takes only the content-addressed key), but needed to tag
    /// and search bake resources by `burst-actions-repo`.
    pub repo: RepoId,
    /// The pinned base AMI to launch a builder from when `bake` misses the
    /// cache (`config.base_ami`, resolved by `commands::bake::run`).
    pub base_ami: String,
    /// Instance type for the builder VM.
    pub builder_instance_type: String,
    /// The rendered provisioning script `bake` wraps into the builder's
    /// user-data.
    pub provisioning_script: String,
}

/// How long a builder (and its kill schedule) may live: generous enough for
/// a from-scratch `apt-get install` + toolchain warm, short enough that a
/// wedged build is capped, not indefinite.
const BUILDER_TTL_HOURS: i64 = 1;
/// Poll interval and deadline while waiting for the builder to reach
/// `Stopped` (provisioning done, or bootstrap-deadline poweroff on failure).
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(15);
const STOP_TIMEOUT_MINUTES: u64 = 25;
/// Poll interval and deadline while waiting for `CreateImage` to finish.
const IMAGE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const IMAGE_TIMEOUT_MINUTES: u64 = 20;

/// True iff `tags` carries the ownership tag `burst-actions=1` — the last
/// check immediately before a destructive call, never trusted from a filter
/// alone.
fn is_burst_tagged(tags: &[aws_sdk_ec2::types::Tag]) -> bool {
    tags.iter()
        .any(|t| t.key() == Some(TAG_BURST) && t.value() == Some("1"))
}

/// Compose the error to surface when a builder must be cleaned up after some
/// prior step failed and there is no kill schedule to fall back on:
/// `terminate_result` is the best-effort tag-verified terminate attempted in
/// response. Fail-loud, truthful residue messaging — if termination itself
/// failed, the message says so plainly and names the instance rather than
/// implying a cleanup that didn't happen.
fn builder_cleanup_error(
    terminate_result: Result<(), Error>,
    builder_id: &str,
    original: Error,
) -> Error {
    match terminate_result {
        Ok(()) => Error::Aws {
            op: "bake",
            message: format!(
                "bake failed after launching builder {builder_id}; builder was terminated ({original})"
            ),
        },
        Err(terminate_err) => Error::Aws {
            op: "bake",
            message: format!(
                "bake failed after launching builder {builder_id} ({original}); termination attempt also failed — builder {builder_id} may still be running: {terminate_err}"
            ),
        },
    }
}

/// Select the superseded generation: every image whose `burst-actions-image-key`
/// tag doesn't match `keep_key`, including images with no readable key tag at
/// all — a tag-verified burst image without a key is stale by definition,
/// never protected by omission. One generation only: `images` is expected to
/// already be filtered to this repo.
pub(crate) fn superseded<'a>(
    images: &'a [(String, Option<String>)],
    keep_key: &str,
) -> Vec<&'a String> {
    images
        .iter()
        .filter(|(_, key)| key.as_deref() != Some(keep_key))
        .map(|(id, _)| id)
        .collect()
}

fn tag_specification(
    resource_type: aws_sdk_ec2::types::ResourceType,
    tags: &[(String, String)],
) -> aws_sdk_ec2::types::TagSpecification {
    let mut builder = aws_sdk_ec2::types::TagSpecification::builder().resource_type(resource_type);
    for (k, v) in tags {
        builder = builder.tags(aws_sdk_ec2::types::Tag::builder().key(k).value(v).build());
    }
    builder.build()
}

impl Cloud for AwsCloud {
    fn launch(&mut self, spec: &LaunchSpec) -> Result<Vec<Instance>, Error> {
        let tags = spec.tags.to_tags();
        let instance_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Instance, &tags);
        let volume_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Volume, &tags);

        let mut market_options = None;
        if spec.spot {
            let spot_options = aws_sdk_ec2::types::SpotMarketOptions::builder()
                .spot_instance_type(aws_sdk_ec2::types::SpotInstanceType::OneTime)
                .instance_interruption_behavior(
                    aws_sdk_ec2::types::InstanceInterruptionBehavior::Terminate,
                )
                .build();
            market_options = Some(
                aws_sdk_ec2::types::InstanceMarketOptionsRequest::builder()
                    .market_type(aws_sdk_ec2::types::MarketType::Spot)
                    .spot_options(spot_options)
                    .build(),
            );
        }

        let user_data_b64 = base64::engine::general_purpose::STANDARD.encode(&spec.user_data);

        self.ctx.runtime.block_on(async {
            let mut delays = retry_delays();
            loop {
                let mut request = self
                    .ctx
                    .ec2
                    .run_instances()
                    .min_count(spec.count as i32)
                    .max_count(spec.count as i32)
                    .image_id(&spec.image_id)
                    .instance_type(aws_sdk_ec2::types::InstanceType::from(spec.instance_type.as_str()))
                    .security_group_ids(&self.substrate.security_group_id)
                    .subnet_id(&self.substrate.subnet_id)
                    .iam_instance_profile(
                        aws_sdk_ec2::types::IamInstanceProfileSpecification::builder()
                            .name(&self.substrate.instance_profile_name)
                            .build(),
                    )
                    .user_data(&user_data_b64)
                    .instance_initiated_shutdown_behavior(aws_sdk_ec2::types::ShutdownBehavior::Terminate)
                    .metadata_options(
                        aws_sdk_ec2::types::InstanceMetadataOptionsRequest::builder()
                            .http_tokens(aws_sdk_ec2::types::HttpTokensState::Required)
                            .build(),
                    )
                    .tag_specifications(instance_tags.clone())
                    .tag_specifications(volume_tags.clone());
                if let Some(m) = market_options.clone() {
                    request = request.instance_market_options(m);
                }

                match request.send().await {
                    Ok(out) => {
                        let instances = out
                            .instances()
                            .iter()
                            .map(|i| -> Result<Instance, Error> {
                                let id = i.instance_id().ok_or_else(|| Error::Aws {
                                    op: "RunInstances",
                                    message: "response instance had no id".to_string(),
                                })?;
                                let state = i.state().and_then(|s| s.name()).ok_or_else(|| Error::Aws {
                                    op: "RunInstances",
                                    message: format!("instance {id} had no state"),
                                })?;
                                Ok(Instance {
                                    id: id.to_string(),
                                    state: map_instance_state(state)?,
                                    tags: tags.to_vec(),
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        return Ok(instances);
                    }
                    Err(e) => {
                        let code = e.code().unwrap_or_default();
                        let message = e.message().unwrap_or_default();
                        if is_iam_propagation_error(code, message) {
                            if let Some(delay) = delays.next() {
                                tokio::time::sleep(delay).await;
                                continue;
                            }
                            return Err(Error::Aws {
                                op: "RunInstances",
                                message: format!(
                                    "IAM role propagation did not settle after 60s — retry `burst bake` ({})",
                                    format_aws_error(&e)
                                ),
                            });
                        }
                        return Err(Error::Aws {
                            op: "RunInstances",
                            message: format_aws_error(&e),
                        });
                    }
                }
            }
        })
    }

    fn terminate(&mut self, ids: &[String]) -> Result<(), Error> {
        if ids.is_empty() {
            return Ok(());
        }
        self.ctx.runtime.block_on(async {
            let describe = self
                .ctx
                .ec2
                .describe_instances()
                .set_instance_ids(Some(ids.to_vec()))
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_BURST}"))
                        .values("1")
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "terminate",
                    message: format_aws_error(&e),
                })?;

            let verified: std::collections::HashSet<String> = describe
                .reservations()
                .iter()
                .flat_map(|r| r.instances())
                .filter_map(|i| i.instance_id().map(str::to_string))
                .collect();

            for id in ids {
                if !verified.contains(id) {
                    return Err(Error::Aws {
                        op: "terminate",
                        message: format!(
                            "refusing to terminate {id}: not verified as carrying burst-actions=1"
                        ),
                    });
                }
            }

            self.ctx
                .ec2
                .terminate_instances()
                .set_instance_ids(Some(ids.to_vec()))
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "TerminateInstances",
                    message: format_aws_error(&e),
                })?;

            Ok(())
        })
    }

    fn list_tagged(&self, repo: &RepoId) -> Result<Vec<Instance>, Error> {
        self.ctx.runtime.block_on(async {
            let mut out = Vec::new();
            let mut next_token: Option<String> = None;
            loop {
                let mut request = self
                    .ctx
                    .ec2
                    .describe_instances()
                    .filters(
                        aws_sdk_ec2::types::Filter::builder()
                            .name(format!("tag:{TAG_BURST}"))
                            .values("1")
                            .build(),
                    )
                    .filters(
                        aws_sdk_ec2::types::Filter::builder()
                            .name(format!("tag:{TAG_REPO}"))
                            .values(repo.to_string())
                            .build(),
                    )
                    .filters(
                        aws_sdk_ec2::types::Filter::builder()
                            .name("instance-state-name")
                            .values("pending")
                            .values("running")
                            .values("shutting-down")
                            .values("stopping")
                            .values("stopped")
                            .build(),
                    );
                if let Some(token) = &next_token {
                    request = request.next_token(token);
                }

                let output = request.send().await.map_err(|e| Error::Aws {
                    op: "DescribeInstances",
                    message: format_aws_error(&e),
                })?;

                for reservation in output.reservations() {
                    for instance in reservation.instances() {
                        let id = instance.instance_id().ok_or_else(|| Error::Aws {
                            op: "DescribeInstances",
                            message: "response instance had no id".to_string(),
                        })?;
                        let state_name =
                            instance
                                .state()
                                .and_then(|s| s.name())
                                .ok_or_else(|| Error::Aws {
                                    op: "DescribeInstances",
                                    message: format!("instance {id} had no state"),
                                })?;
                        let tags = instance
                            .tags()
                            .iter()
                            .filter_map(|t| Some((t.key()?.to_string(), t.value()?.to_string())))
                            .collect();
                        out.push(Instance {
                            id: id.to_string(),
                            state: map_instance_state(state_name)?,
                            tags,
                        });
                    }
                }

                next_token = output.next_token().map(str::to_string);
                if next_token.is_none() {
                    break;
                }
            }
            Ok(out)
        })
    }

    fn arm_kill(&mut self, instance_id: &str, at: DateTime<Utc>) -> Result<(), Error> {
        let input = serde_json::to_string(&serde_json::json!({ "InstanceIds": [instance_id] }))
            .expect("static shape always serializes");

        self.ctx.runtime.block_on(async {
            let mut delays = retry_delays();
            loop {
                let target = aws_sdk_scheduler::types::Target::builder()
                    .arn("arn:aws:scheduler:::aws-sdk:ec2:terminateInstances")
                    .role_arn(&self.substrate.scheduler_role_arn)
                    .input(input.clone())
                    .build()
                    .map_err(|e| Error::Aws {
                        op: "CreateSchedule",
                        message: format_aws_error(&e),
                    })?;

                let flexible_time_window = aws_sdk_scheduler::types::FlexibleTimeWindow::builder()
                    .mode(aws_sdk_scheduler::types::FlexibleTimeWindowMode::Off)
                    .build()
                    .map_err(|e| Error::Aws {
                        op: "CreateSchedule",
                        message: format_aws_error(&e),
                    })?;

                let result = self
                    .ctx
                    .scheduler
                    .create_schedule()
                    .name(kill_schedule_name(instance_id))
                    .group_name("default")
                    .schedule_expression(at_expression(at))
                    .schedule_expression_timezone("UTC")
                    .flexible_time_window(flexible_time_window)
                    .action_after_completion(aws_sdk_scheduler::types::ActionAfterCompletion::Delete)
                    .target(target)
                    .send()
                    .await;

                match result {
                    Ok(_) => return Ok(()),
                    // Schedule already exists — arming is idempotent per instance.
                    Err(e)
                        if e.as_service_error()
                            .is_some_and(|s| s.is_conflict_exception()) =>
                    {
                        return Ok(());
                    }
                    Err(e)
                        if e.as_service_error().is_some_and(|s| {
                            s.is_validation_exception()
                                && s.message().unwrap_or_default().contains("assume")
                        }) =>
                    {
                        if let Some(delay) = delays.next() {
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Err(Error::Aws {
                            op: "CreateSchedule",
                            message: format!(
                                "scheduler role propagation did not settle after 60s — retry `burst up` ({})",
                                format_aws_error(&e)
                            ),
                        });
                    }
                    Err(e) => {
                        return Err(Error::Aws {
                            op: "CreateSchedule",
                            message: format_aws_error(&e),
                        });
                    }
                }
            }
        })
    }

    fn bake(&mut self, key: &str) -> Result<String, Error> {
        if let Some(id) = self.describe_image_by_key(key)? {
            println!("image cache hit: {id} ({key})");
            return Ok(id);
        }

        let expires = Utc::now() + chrono::Duration::hours(BUILDER_TTL_HOURS);
        let builder_id = self.launch_builder(expires)?;
        // Kill-armed before any waiting begins: a SIGKILLed CLI leaks only a
        // schedule-reaped builder, never a runaway one. If arming itself
        // fails, there is no schedule to fall back on — terminate the
        // builder right here (best-effort, tag-verified) rather than
        // leaving an unfenced instance behind.
        if let Err(arm_err) = self.arm_kill(&builder_id, expires) {
            return Err(builder_cleanup_error(
                self.terminate(std::slice::from_ref(&builder_id)),
                &builder_id,
                arm_err,
            ));
        }

        // Any failure between launch and a finished image gets the same
        // treatment as an arm_kill failure above: best-effort terminate the
        // builder now rather than leaving it to idle until the schedule
        // fires (the schedule stays armed as the backstop and self-deletes
        // after firing on the already-dead instance).
        let image_id = match self.build_image_from_builder(&builder_id, key) {
            Ok(id) => id,
            Err(err) => {
                return Err(builder_cleanup_error(
                    self.terminate(std::slice::from_ref(&builder_id)),
                    &builder_id,
                    err,
                ));
            }
        };

        self.terminate(std::slice::from_ref(&builder_id))?;
        self.delete_kill_schedule(&builder_id)?;

        self.cleanup_superseded(key)?;

        Ok(image_id)
    }
}

impl AwsCloud {
    /// The wait → CreateImage → wait sequence between builder launch and a
    /// usable AMI, factored out so `bake` can clean up the builder on any
    /// error in it.
    fn build_image_from_builder(&mut self, builder_id: &str, key: &str) -> Result<String, Error> {
        self.wait_for_stopped(builder_id)?;
        let image_id = self.create_image(builder_id, key)?;
        self.wait_for_image_available(&image_id)?;
        Ok(image_id)
    }

    /// Cache check: an available AMI we own, tagged for this repo's key.
    /// Get-or-create semantics — a hit short-circuits before any builder is
    /// launched.
    fn describe_image_by_key(&self, key: &str) -> Result<Option<String>, Error> {
        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .describe_images()
                .owners("self")
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_BURST}"))
                        .values("1")
                        .build(),
                )
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_IMAGE_KEY}"))
                        .values(key)
                        .build(),
                )
                // Repo-scoped like cleanup_superseded: without this, repo A
                // could cache-hit repo B's AMI — which B's next rebake then
                // deletes out from under A.
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_REPO}"))
                        .values(self.repo.to_string())
                        .build(),
                )
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name("state")
                        .values("available")
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeImages(cache check)",
                    message: format_aws_error(&e),
                })?;
            Ok(out
                .images()
                .first()
                .and_then(|i| i.image_id())
                .map(str::to_string))
        })
    }

    /// Launch the builder instance: tag triple, `shutdown_behavior = Stop`
    /// (the one deliberate exception — `CreateImage` needs a stopped
    /// instance), IMDSv2, our SG/subnet/profile, wrapped provisioning
    /// user-data.
    fn launch_builder(&self, expires: DateTime<Utc>) -> Result<String, Error> {
        let tags = crate::schema::TagSpec {
            repo: self.repo.clone(),
            expires,
        }
        .to_tags();
        let instance_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Instance, &tags);
        let volume_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Volume, &tags);

        let wrapped = crate::payload::wrap_provision_for_bake(&self.provisioning_script)?;
        let user_data_b64 = base64::engine::general_purpose::STANDARD.encode(wrapped.as_bytes());

        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .run_instances()
                .min_count(1)
                .max_count(1)
                .image_id(&self.base_ami)
                .instance_type(aws_sdk_ec2::types::InstanceType::from(
                    self.builder_instance_type.as_str(),
                ))
                .security_group_ids(&self.substrate.security_group_id)
                .subnet_id(&self.substrate.subnet_id)
                .iam_instance_profile(
                    aws_sdk_ec2::types::IamInstanceProfileSpecification::builder()
                        .name(&self.substrate.instance_profile_name)
                        .build(),
                )
                .user_data(&user_data_b64)
                .instance_initiated_shutdown_behavior(aws_sdk_ec2::types::ShutdownBehavior::Stop)
                .metadata_options(
                    aws_sdk_ec2::types::InstanceMetadataOptionsRequest::builder()
                        .http_tokens(aws_sdk_ec2::types::HttpTokensState::Required)
                        .build(),
                )
                .tag_specifications(instance_tags)
                .tag_specifications(volume_tags)
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "RunInstances(builder)",
                    message: format_aws_error(&e),
                })?;

            out.instances()
                .first()
                .and_then(|i| i.instance_id())
                .map(str::to_string)
                .ok_or_else(|| Error::Aws {
                    op: "RunInstances(builder)",
                    message: "response had no instance id".to_string(),
                })
        })
    }

    fn describe_instance_state(&self, instance_id: &str) -> Result<InstanceState, Error> {
        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .describe_instances()
                .instance_ids(instance_id)
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeInstances(builder)",
                    message: format_aws_error(&e),
                })?;
            let state = out
                .reservations()
                .iter()
                .flat_map(|r| r.instances())
                .next()
                .and_then(|i| i.state())
                .and_then(|s| s.name())
                .ok_or_else(|| Error::Aws {
                    op: "DescribeInstances(builder)",
                    message: format!("builder {instance_id} not found"),
                })?;
            map_instance_state(state)
        })
    }

    /// Poll until the builder reaches `Stopped` (provisioning succeeded, or
    /// the bootstrap-deadline timer powered it off on failure). Past the
    /// deadline: terminate (tag-verified) and delete the kill schedule
    /// first — exactly what the timeout error promises — then fail loud.
    fn wait_for_stopped(&mut self, builder_id: &str) -> Result<(), Error> {
        let deadline = std::time::Instant::now() + Duration::from_secs(STOP_TIMEOUT_MINUTES * 60);
        loop {
            if self.describe_instance_state(builder_id)? == InstanceState::Stopped {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                self.terminate(&[builder_id.to_string()])?;
                self.delete_kill_schedule(builder_id)?;
                return Err(Error::BakeTimeout {
                    instance_id: builder_id.to_string(),
                    minutes: STOP_TIMEOUT_MINUTES,
                });
            }
            std::thread::sleep(STOP_POLL_INTERVAL);
        }
    }

    /// `CreateImage`, tagging both the image and its snapshot(s) with the
    /// ownership triple plus the image key.
    fn create_image(&self, builder_id: &str, key: &str) -> Result<String, Error> {
        let tags: Vec<(String, String)> = vec![
            (TAG_BURST.to_string(), "1".to_string()),
            (TAG_REPO.to_string(), self.repo.to_string()),
            (TAG_IMAGE_KEY.to_string(), key.to_string()),
        ];
        let image_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Image, &tags);
        let snapshot_tags = tag_specification(aws_sdk_ec2::types::ResourceType::Snapshot, &tags);

        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .create_image()
                .instance_id(builder_id)
                .name(format!("burst-actions-{key}"))
                .tag_specifications(image_tags)
                .tag_specifications(snapshot_tags)
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "CreateImage",
                    message: format_aws_error(&e),
                })?;
            out.image_id()
                .map(str::to_string)
                .ok_or_else(|| Error::Aws {
                    op: "CreateImage",
                    message: "response had no image id".to_string(),
                })
        })
    }

    fn describe_image_state(
        &self,
        image_id: &str,
    ) -> Result<aws_sdk_ec2::types::ImageState, Error> {
        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .describe_images()
                .image_ids(image_id)
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeImages(await available)",
                    message: format_aws_error(&e),
                })?;
            out.images()
                .first()
                .and_then(|i| i.state())
                .cloned()
                .ok_or_else(|| Error::Aws {
                    op: "DescribeImages(await available)",
                    message: format!("image {image_id} not found"),
                })
        })
    }

    fn wait_for_image_available(&self, image_id: &str) -> Result<(), Error> {
        let deadline = std::time::Instant::now() + Duration::from_secs(IMAGE_TIMEOUT_MINUTES * 60);
        loop {
            let state = self.describe_image_state(image_id)?;
            if state == aws_sdk_ec2::types::ImageState::Available {
                return Ok(());
            }
            if state == aws_sdk_ec2::types::ImageState::Failed
                || state == aws_sdk_ec2::types::ImageState::Error
            {
                return Err(Error::Aws {
                    op: "CreateImage",
                    message: format!("image {image_id} entered state {state:?}"),
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Aws {
                    op: "CreateImage",
                    message: format!(
                        "image {image_id} did not reach 'available' within {IMAGE_TIMEOUT_MINUTES} min"
                    ),
                });
            }
            std::thread::sleep(IMAGE_POLL_INTERVAL);
        }
    }

    /// Best-effort-but-checked delete of a builder's kill schedule: already
    /// gone (fired, or never armed) is not an error.
    fn delete_kill_schedule(&self, instance_id: &str) -> Result<(), Error> {
        self.ctx.runtime.block_on(async {
            match self
                .ctx
                .scheduler
                .delete_schedule()
                .name(kill_schedule_name(instance_id))
                .group_name("default")
                .send()
                .await
            {
                Ok(_) => Ok(()),
                Err(e)
                    if e.as_service_error()
                        .is_some_and(|s| s.is_resource_not_found_exception()) =>
                {
                    Ok(())
                }
                Err(e) => Err(Error::Aws {
                    op: "DeleteSchedule",
                    message: format_aws_error(&e),
                }),
            }
        })
    }

    /// All available images we own, tagged `burst-actions=1` for this repo —
    /// the search space `superseded` is a pure function over.
    fn list_repo_images(&self) -> Result<Vec<aws_sdk_ec2::types::Image>, Error> {
        self.ctx.runtime.block_on(async {
            let out = self
                .ctx
                .ec2
                .describe_images()
                .owners("self")
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_BURST}"))
                        .values("1")
                        .build(),
                )
                .filters(
                    aws_sdk_ec2::types::Filter::builder()
                        .name(format!("tag:{TAG_REPO}"))
                        .values(self.repo.to_string())
                        .build(),
                )
                .send()
                .await
                .map_err(|e| Error::Aws {
                    op: "DescribeImages(supersession)",
                    message: format_aws_error(&e),
                })?;
            Ok(out.images().to_vec())
        })
    }

    /// One-generation GC: deregister and delete the snapshots of every image
    /// this repo owns whose image-key isn't `keep_key`, re-verifying
    /// ownership on each immediately before the destructive calls.
    fn cleanup_superseded(&mut self, keep_key: &str) -> Result<(), Error> {
        let images = self.list_repo_images()?;
        let pairs: Vec<(String, Option<String>)> = images
            .iter()
            .filter_map(|img| {
                let id = img.image_id()?.to_string();
                let key_tag = img
                    .tags()
                    .iter()
                    .find(|t| t.key() == Some(TAG_IMAGE_KEY))
                    .and_then(|t| t.value())
                    .map(str::to_string);
                Some((id, key_tag))
            })
            .collect();

        for stale_id in superseded(&pairs, keep_key) {
            let image = images
                .iter()
                .find(|img| img.image_id() == Some(stale_id.as_str()))
                .ok_or_else(|| Error::Aws {
                    op: "DeregisterImage",
                    message: format!(
                        "superseded image {stale_id} vanished from the describe result"
                    ),
                })?;
            if !is_burst_tagged(image.tags()) {
                return Err(Error::Aws {
                    op: "DeregisterImage",
                    message: format!(
                        "refusing to deregister {stale_id}: not verified as carrying burst-actions=1"
                    ),
                });
            }
            let snapshot_ids: Vec<String> = image
                .block_device_mappings()
                .iter()
                .filter_map(|m| m.ebs().and_then(|e| e.snapshot_id()).map(str::to_string))
                .collect();

            self.ctx.runtime.block_on(async {
                self.ctx
                    .ec2
                    .deregister_image()
                    .image_id(stale_id)
                    .send()
                    .await
                    .map_err(|e| Error::Aws {
                        op: "DeregisterImage",
                        message: format_aws_error(&e),
                    })?;
                for snapshot_id in &snapshot_ids {
                    self.ctx
                        .ec2
                        .delete_snapshot()
                        .snapshot_id(snapshot_id)
                        .send()
                        .await
                        .map_err(|e| Error::Aws {
                            op: "DeleteSnapshot",
                            message: format_aws_error(&e),
                        })?;
                }
                Ok::<(), Error>(())
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_region_wins_over_chain() {
        assert_eq!(
            effective_region(Some("us-west-2"), Some("us-east-1")).unwrap(),
            "us-west-2"
        );
    }

    #[test]
    fn chain_region_used_when_config_absent() {
        assert_eq!(
            effective_region(None, Some("us-east-1")).unwrap(),
            "us-east-1"
        );
    }

    #[test]
    fn neither_present_is_region_missing() {
        assert!(matches!(
            effective_region(None, None),
            Err(Error::RegionMissing)
        ));
    }

    #[test]
    fn detects_dispatch_failure_missing_credentials() {
        assert!(is_credentials_error(
            "dispatch failure: failed to load credentials from any provider in the chain"
        ));
    }

    #[test]
    fn detects_invalid_client_token_id() {
        assert!(is_credentials_error(
            "service error: InvalidClientTokenId: The security token included in the request is invalid"
        ));
    }

    #[test]
    fn detects_auth_failure_and_related_codes() {
        for code in [
            "AuthFailure",
            "UnrecognizedClientException",
            "SignatureDoesNotMatch",
            "ExpiredToken",
        ] {
            assert!(is_credentials_error(code), "expected {code} to be detected");
        }
    }

    #[test]
    fn unrelated_error_is_not_a_credentials_error() {
        assert!(!is_credentials_error(
            "service error: InvalidVpcID.NotFound: vpc-1234 does not exist"
        ));
    }

    #[derive(Debug)]
    struct SourcedError {
        message: &'static str,
        source: Option<Box<SourcedError>>,
    }

    impl std::fmt::Display for SourcedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for SourcedError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|s| s as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn format_aws_error_surfaces_full_chain() {
        let err = SourcedError {
            message: "dispatch failure",
            source: Some(Box::new(SourcedError {
                message: "no credentials in the property bag",
                source: None,
            })),
        };
        let formatted = format_aws_error(&err);
        assert!(formatted.contains("dispatch failure"));
        assert!(formatted.contains("no credentials in the property bag"));
        assert!(formatted.contains("configure AWS credentials: env vars or `aws configure`"));
    }

    #[test]
    fn trust_policy_names_exact_service_principal() {
        let doc: serde_json::Value =
            serde_json::from_str(&trust_policy("ec2.amazonaws.com")).unwrap();
        assert_eq!(
            doc,
            serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": "ec2.amazonaws.com" },
                    "Action": "sts:AssumeRole"
                }]
            })
        );
    }

    #[test]
    fn trust_policy_varies_by_service() {
        let doc: serde_json::Value =
            serde_json::from_str(&trust_policy("scheduler.amazonaws.com")).unwrap();
        assert_eq!(
            doc["Statement"][0]["Principal"]["Service"],
            "scheduler.amazonaws.com"
        );
    }

    #[test]
    fn scheduler_kill_policy_is_tag_fenced_terminate_only() {
        let doc: serde_json::Value = serde_json::from_str(&scheduler_kill_policy()).unwrap();
        assert_eq!(
            doc,
            serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "ec2:TerminateInstances",
                    "Resource": "arn:aws:ec2:*:*:instance/*",
                    "Condition": {
                        "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
                    }
                }]
            })
        );
    }

    #[test]
    fn iam_propagation_error_detected() {
        assert!(is_iam_propagation_error(
            "InvalidParameterValue",
            "Invalid IAM Instance Profile name"
        ));
    }

    #[test]
    fn iam_propagation_error_requires_matching_message() {
        assert!(!is_iam_propagation_error(
            "InvalidParameterValue",
            "some other message"
        ));
    }

    #[test]
    fn kill_schedule_name_is_prefixed_by_instance_id() {
        assert_eq!(kill_schedule_name("i-0abc"), "burst-actions-i-0abc");
    }

    #[test]
    fn at_expression_has_no_zone_suffix() {
        let at = DateTime::parse_from_rfc3339("2026-08-08T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(at_expression(at), "at(2026-08-08T18:00:00)");
    }

    #[test]
    fn kill_target_input_round_trips() {
        let input =
            serde_json::to_string(&serde_json::json!({ "InstanceIds": ["i-0abc"] })).unwrap();
        let value: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(value, serde_json::json!({ "InstanceIds": ["i-0abc"] }));
    }

    #[test]
    fn iam_propagation_error_requires_matching_code() {
        assert!(!is_iam_propagation_error(
            "UnauthorizedOperation",
            "Invalid IAM Instance Profile name"
        ));
    }

    #[test]
    fn retry_delays_are_six_entries_summing_to_59s() {
        let delays: Vec<Duration> = retry_delays().collect();
        assert_eq!(delays.len(), 6);
        assert_eq!(delays.iter().sum::<Duration>(), Duration::from_secs(59));
    }

    #[test]
    fn instance_state_mapping_covers_all_documented_names() {
        use aws_sdk_ec2::types::InstanceStateName as N;
        assert_eq!(
            map_instance_state(&N::Pending).unwrap(),
            InstanceState::Pending
        );
        assert_eq!(
            map_instance_state(&N::Running).unwrap(),
            InstanceState::Running
        );
        assert_eq!(
            map_instance_state(&N::ShuttingDown).unwrap(),
            InstanceState::ShuttingDown
        );
        assert_eq!(
            map_instance_state(&N::Terminated).unwrap(),
            InstanceState::Terminated
        );
        assert_eq!(
            map_instance_state(&N::Stopping).unwrap(),
            InstanceState::Stopping
        );
        assert_eq!(
            map_instance_state(&N::Stopped).unwrap(),
            InstanceState::Stopped
        );
    }

    #[test]
    fn instance_state_mapping_errors_on_unrecognized_name() {
        let weird = aws_sdk_ec2::types::InstanceStateName::from("weird");
        assert!(matches!(map_instance_state(&weird), Err(Error::Aws { .. })));
    }

    #[test]
    fn builder_cleanup_error_names_instance_when_terminate_succeeds() {
        let err = builder_cleanup_error(
            Ok(()),
            "i-0abc",
            Error::Aws {
                op: "CreateSchedule",
                message: "scheduler role propagation did not settle".into(),
            },
        );
        let msg = err.to_string();
        assert!(msg.contains("i-0abc"), "{msg}");
        assert!(msg.contains("terminated"), "{msg}");
        assert!(
            msg.contains("scheduler role propagation did not settle"),
            "{msg}"
        );
    }

    #[test]
    fn builder_cleanup_error_admits_builder_may_still_be_running() {
        let err = builder_cleanup_error(
            Err(Error::Aws {
                op: "TerminateInstances",
                message: "throttled".into(),
            }),
            "i-0abc",
            Error::Aws {
                op: "CreateSchedule",
                message: "scheduler role propagation did not settle".into(),
            },
        );
        let msg = err.to_string();
        // Fail-loud, truthful residue: must name the instance and admit it
        // may still be running, never imply cleanup succeeded.
        assert!(msg.contains("i-0abc"), "{msg}");
        assert!(msg.contains("may still be running"), "{msg}");
        assert!(msg.contains("throttled"), "{msg}");
        assert!(
            msg.contains("scheduler role propagation did not settle"),
            "{msg}"
        );
    }

    #[test]
    fn superseded_keeps_only_the_matching_key() {
        let images = vec![
            ("ami-old".to_string(), Some("v1-old".to_string())),
            ("ami-new".to_string(), Some("v1-new".to_string())),
        ];
        let stale = superseded(&images, "v1-new");
        assert_eq!(stale, vec![&"ami-old".to_string()]);
    }

    #[test]
    fn superseded_treats_missing_key_tag_as_stale() {
        let images = vec![
            ("ami-no-key".to_string(), None),
            ("ami-new".to_string(), Some("v1-new".to_string())),
        ];
        let stale = superseded(&images, "v1-new");
        assert_eq!(stale, vec![&"ami-no-key".to_string()]);
    }

    #[test]
    fn superseded_is_empty_when_all_match() {
        let images = vec![("ami-new".to_string(), Some("v1-new".to_string()))];
        assert!(superseded(&images, "v1-new").is_empty());
    }

    #[test]
    fn is_burst_tagged_requires_exact_key_and_value() {
        let tagged = [aws_sdk_ec2::types::Tag::builder()
            .key(TAG_BURST)
            .value("1")
            .build()];
        assert!(is_burst_tagged(&tagged));
        let untagged: [aws_sdk_ec2::types::Tag; 0] = [];
        assert!(!is_burst_tagged(&untagged));
    }

    #[test]
    fn format_aws_error_leaves_non_credential_errors_unmodified() {
        let err = SourcedError {
            message: "service error: InvalidVpcID.NotFound",
            source: None,
        };
        let formatted = format_aws_error(&err);
        assert!(formatted.starts_with("service error: InvalidVpcID.NotFound"));
        assert!(!formatted.contains("configure AWS credentials"));
    }
}

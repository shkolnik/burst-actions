use crate::error::Error;

/// Live AWS context: a single-thread runtime plus the clients this crate needs.
///
/// Construction (`connect`) and the VPC probe both make network calls, so
/// nothing in this module runs at `cargo test` time unless explicitly
/// invoked; only [`effective_region`] is unit-tested.
pub struct AwsContext {
    runtime: tokio::runtime::Runtime,
    ec2: aws_sdk_ec2::Client,
    #[allow(dead_code)] // wired up by later tasks (arm_kill)
    scheduler: aws_sdk_scheduler::Client,
    #[allow(dead_code)] // wired up by later tasks (bake role setup)
    iam: aws_sdk_iam::Client,
    #[allow(dead_code)] // wired up by later tasks (quota/budget checks)
    budgets: aws_sdk_budgets::Client,
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

        Ok(AwsContext {
            runtime,
            ec2,
            scheduler,
            iam,
            budgets,
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
                    message: e.to_string(),
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
                    message: e.to_string(),
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
}

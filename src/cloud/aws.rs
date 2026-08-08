use crate::error::Error;
use aws_smithy_types::error::display::DisplayErrorContext;

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

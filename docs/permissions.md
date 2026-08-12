# Permissions: what burst needs from AWS and GitHub

The exact credentials any user of the tool must provide — nothing more. Derived from the
approved design (`design-proposal.md` §3 lifecycle, §5 SDK surface); every statement maps to a
specific call the tool makes. Drafted phase 2 (2026-08-08), unverified against a live account
until the phase-2 gate runs; expect wording fixes (AWS resource-level support has quirks) but no
scope growth.

## AWS IAM policy (for the invoking user/role)

Design principle: **everything mutating is fenced to burst-owned resources** — by the `burst-actions=1`
tag where the action supports tag conditions, by the `burst-actions-*` name prefix where it doesn't
(IAM, Scheduler, Budgets). Reads that AWS cannot resource-scope (`Describe*`) are the only `*`
resources.

Sizing the root volume (`volume_gb` and friends) needs no policy change: it is the same
`RunInstances` call with a block-device mapping, and the volume still carries the request tag
`LaunchTaggedOnly` requires. Verified live at 750 GiB on 2026-08-12.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "ReadOnlyDescribes",
      "Effect": "Allow",
      "Action": [
        "ec2:DescribeInstances",
        "ec2:DescribeInstanceStatus",
        "ec2:DescribeImages",
        "ec2:DescribeSnapshots",
        "ec2:DescribeSecurityGroups",
        "ec2:DescribeVpcs",
        "ec2:DescribeSubnets",
        "ec2:DescribeTags",
        "ec2:DescribeInstanceTypes"
      ],
      "Resource": "*"
    },
    {
      "Sid": "LaunchTaggedOnly",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": [
        "arn:aws:ec2:*:*:instance/*",
        "arn:aws:ec2:*:*:volume/*"
      ],
      "Condition": {
        "StringEquals": { "aws:RequestTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "LaunchReferencedResources",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": [
        "arn:aws:ec2:*:*:image/*",
        "arn:aws:ec2:*:*:network-interface/*",
        "arn:aws:ec2:*:*:subnet/*",
        "arn:aws:ec2:*:*:key-pair/*"
      ]
    },
    {
      "Sid": "LaunchIntoOurSecurityGroupOnly",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": "arn:aws:ec2:*:*:security-group/*",
      "Condition": {
        "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "TagOnlyAtCreation",
      "Effect": "Allow",
      "Action": "ec2:CreateTags",
      "Resource": "*",
      "Condition": {
        "StringEquals": {
          "ec2:CreateAction": ["RunInstances", "CreateImage", "CreateSecurityGroup"],
          "aws:RequestTag/burst-actions": "1"
        }
      }
    },
    {
      "Sid": "TerminateOursOnly",
      "Effect": "Allow",
      "Action": "ec2:TerminateInstances",
      "Resource": "arn:aws:ec2:*:*:instance/*",
      "Condition": {
        "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "BakeImageFromOurBuilder",
      "Effect": "Allow",
      "Action": "ec2:CreateImage",
      "Resource": "arn:aws:ec2:*:*:instance/*",
      "Condition": {
        "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "BakeImageArtifacts",
      "Effect": "Allow",
      "Action": "ec2:CreateImage",
      "Resource": [
        "arn:aws:ec2:*:*:image/*",
        "arn:aws:ec2:*:*:snapshot/*"
      ],
      "Condition": {
        "StringEquals": { "aws:RequestTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "DeleteSupersededImage",
      "Effect": "Allow",
      "Action": ["ec2:DeregisterImage", "ec2:DeleteSnapshot"],
      "Resource": [
        "arn:aws:ec2:*:*:image/*",
        "arn:aws:ec2:*:*:snapshot/*"
      ],
      "Condition": {
        "StringEquals": { "aws:ResourceTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "CreateZeroInboundSecurityGroup",
      "Effect": "Allow",
      "Action": "ec2:CreateSecurityGroup",
      "Resource": "arn:aws:ec2:*:*:security-group/*",
      "Condition": {
        "StringEquals": { "aws:RequestTag/burst-actions": "1" }
      }
    },
    {
      "Sid": "CreateSecurityGroupInVpc",
      "Effect": "Allow",
      "Action": "ec2:CreateSecurityGroup",
      "Resource": "arn:aws:ec2:*:*:vpc/*"
    },
    {
      "Sid": "OneShotKillSchedules",
      "Effect": "Allow",
      "Action": [
        "scheduler:CreateSchedule",
        "scheduler:GetSchedule",
        "scheduler:DeleteSchedule"
      ],
      "Resource": "arn:aws:scheduler:*:*:schedule/default/burst-actions-*"
    },
    {
      "Sid": "ListKillSchedules",
      "Effect": "Allow",
      "Action": "scheduler:ListSchedules",
      "Resource": "*"
    },
    {
      "Sid": "SubstrateRoles",
      "Effect": "Allow",
      "Action": [
        "iam:CreateRole",
        "iam:GetRole",
        "iam:TagRole",
        "iam:PutRolePolicy",
        "iam:GetRolePolicy",
        "iam:CreateInstanceProfile",
        "iam:GetInstanceProfile",
        "iam:AddRoleToInstanceProfile"
      ],
      "Resource": [
        "arn:aws:iam::*:role/burst-actions-*",
        "arn:aws:iam::*:instance-profile/burst-actions-*"
      ]
    },
    {
      "Sid": "PassOurRolesToTheirServices",
      "Effect": "Allow",
      "Action": "iam:PassRole",
      "Resource": "arn:aws:iam::*:role/burst-actions-*",
      "Condition": {
        "StringEquals": {
          "iam:PassedToService": ["ec2.amazonaws.com", "scheduler.amazonaws.com"]
        }
      }
    },
    {
      "Sid": "QuotaCheck",
      "Effect": "Allow",
      "Action": "servicequotas:GetServiceQuota",
      "Resource": "*"
    },
    {
      "Sid": "OptInBudgetAlarm",
      "Effect": "Allow",
      "Action": ["budgets:ViewBudget", "budgets:ModifyBudget"],
      "Resource": "arn:aws:budgets::*:budget/burst-actions-*"
    }
  ]
}
```

What each statement serves:

| Sid | Design element |
|---|---|
| ReadOnlyDescribes | `list_tagged`, sweep, adoption, base-AMI lookup, default-VPC probe |
| LaunchTaggedOnly / LaunchReferencedResources | `RunInstances` — the tag condition makes invariant 2 ("tag or it doesn't exist") IAM-enforced, not just tool discipline: an untagged launch is *denied* |
| TagOnlyAtCreation | tags land atomically via `TagSpecifications`; no freestanding retagging of arbitrary resources |
| TerminateOursOnly | `down`, sweep, superseded-builder cleanup — cannot touch anything unburst-tagged |
| Bake\* / DeleteSupersededImage | `burst bake` and one-generation image GC |
| CreateZeroInboundSecurityGroup | `ensure_substrate()` (a new SG is zero-inbound by default; no rule-editing permission needed) |
| OneShotKillSchedules | cleanup layer 1 |
| ListKillSchedules | sweep's orphan-schedule scan — split out because AWS evaluates `ListSchedules` against `schedule/*/*`, never a name-prefixed ARN (live-verified: prefix-scoped grant is denied) |
| SubstrateRoles / PassOurRoles | `ensure_substrate()`: the near-empty instance-profile role (trust: ec2) and the Scheduler execution role (trust: scheduler; policy: `TerminateInstances` where `burst-actions=1`) |
| QuotaCheck / ec2:DescribeInstanceTypes | the vCPU-quota warning (decision 9) |
| OptInBudgetAlarm | cleanup layer 5 (opt-in; omit this statement if declining the alarm) |

### Known limits, stated plainly

- **The create/modify fence is the `burst-actions=1` tag** (`aws:RequestTag` on creation,
  `aws:ResourceTag` on mutation): every resource this key creates must carry it, and the key can
  only terminate/delete/retag what carries it. Four launch-time *references* cannot be
  tag-fenced and stay open: the base AMI (Canonical's, untagged — needed to launch the bake
  builder), the default subnet, the ENI RunInstances creates implicitly, and the optional
  `--ssh-key` key-pair. All are launch inputs, not mutation targets. The security group
  reference *is* fenced — instances can only launch into the burst-tagged SG.

- **`SubstrateRoles` + `PassRole` is the sharp edge.** `iam:CreateRole` + `iam:PutRolePolicy`
  scoped to `burst-actions-*` still lets the holder author a `burst-actions-*` role with *any* inline policy and
  hand it to EC2 — a privilege-escalation path if the credential leaks. Tag/name scoping cannot
  close it; only a [permissions boundary](https://docs.aws.amazon.com/IAM/latest/UserGuide/access_policies_boundaries.html)
  can (add `"Condition": {"StringEquals": {"iam:PermissionsBoundary": "<boundary-arn>"}}` to
  `iam:CreateRole` and have the tool set it). Deferred: worth doing before public release;
  overkill for the maintainer's own account where the credential already belongs to the
  account owner.
- The two roles the tool *creates* are separate from this policy and much smaller: the instance
  profile is near-empty (grows exactly one S3-prefix statement if phase-2 sccache lands); the
  Scheduler role can only terminate `burst-actions=1` instances.
- Regional pinning: replace the `*` region in the ARNs with the configured region for a tighter
  fence; left `*` here since the region is user-config.

## GitHub: fine-grained PAT, scoped to the target repo

| Permission | Level | Serves |
|---|---|---|
| Administration | Read & write | `generate-jitconfig` minting; never-connected registration cleanup; reading the fork-approval Actions setting (invariant 5 preflight) |
| Actions | Read | `--auto`'s queued-run/job listing |
| Metadata | Read | implied baseline |

Nothing org-level; nothing on other repos. A classic PAT also works mechanically (the runner
endpoints accept `repo` scope) but `repo` is full control of *every* repo the account touches —
the design specifies fine-grained for the same least-privilege reason as the tag-fenced AWS
policy above.

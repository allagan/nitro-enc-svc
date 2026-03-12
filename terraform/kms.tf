# ── DEK key — encrypts/decrypts the Data Encryption Key ──────────────────────

resource "aws_kms_key" "enclave_dek" {
  description             = "nitro-enc-svc ${var.environment}: encrypts the Data Encryption Key (DEK)"
  enable_key_rotation     = true
  deletion_window_in_days = 30

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat(
      [
        # Root/admin access — always present
        {
          Sid    = "AllowRootAdmin"
          Effect = "Allow"
          Principal = {
            AWS = "arn:aws:iam::${var.account_id}:root"
          }
          Action   = "kms:*"
          Resource = "*"
        },
        # CodeBuild — needs to wrap a newly generated DEK during CI
        {
          Sid    = "AllowCodeBuild"
          Effect = "Allow"
          Principal = {
            AWS = aws_iam_role.codebuild.arn
          }
          Action = [
            "kms:GenerateDataKey",
            "kms:Encrypt",
            "kms:DescribeKey",
          ]
          Resource = "*"
        },
      ],
      # DEV: standard Decrypt allowed from the enclave node IAM role.
      # TODO: Once NSM attested decrypt is implemented in dek/mod.rs, change
      # this back to a RecipientAttestation:PCR0 condition so that only a
      # genuine enclave with the correct image measurement can decrypt the DEK.
      # When kms_enclave_pcr0 is set: enforce NSM attestation on Decrypt so only
      # an enclave whose EIF produces the expected PCR0 measurement can decrypt
      # the DEK.  PCR0 is a SHA-384 hash of the enclave image file contents and
      # changes on every build.  Update terraform.tfvars after each CodeBuild run.
      #
      # The KMS key policy condition kms:RecipientAttestation:PCR0 is evaluated
      # when the KMS Decrypt call includes a RecipientAttestation parameter
      # containing a valid NSM attestation document (see dek/mod.rs for the
      # aws-nitro-enclaves-sdk-rust integration).
      #
      # When kms_enclave_pcr0 is empty (dev mode): standard IAM Decrypt is
      # allowed without attestation — the DEK can be fetched by any process
      # with the enclave_node IAM role.
      [
        merge(
          {
            Sid    = var.kms_enclave_pcr0 != "" ? "AllowEnclaveDecryptWithAttestation" : "AllowEnclaveDecryptDevMode"
            Effect = "Allow"
            Principal = {
              AWS = aws_iam_role.enclave_node.arn
            }
            Action   = ["kms:Decrypt", "kms:DescribeKey"]
            Resource = "*"
          },
          # When PCR0 is set, add the RecipientAttestation condition so only
          # a genuine enclave with that image measurement can decrypt the DEK.
          var.kms_enclave_pcr0 != "" ? {
            Condition = {
              StringEqualsIgnoreCase = {
                "kms:RecipientAttestation:PCR0" = var.kms_enclave_pcr0
              }
            }
          } : {}
        )
      ]
    )
  })

  tags = {
    Name = "nitro-enc-svc-${var.environment}-dek"
  }
}

resource "aws_kms_alias" "enclave_dek" {
  name          = "alias/nitro-enc-svc/${var.environment}/dek"
  target_key_id = aws_kms_key.enclave_dek.key_id
}

# ── EKS Secrets key — encrypts Kubernetes Secrets in etcd ────────────────────

resource "aws_kms_key" "eks_secrets" {
  description             = "nitro-enc-svc ${var.environment}: encrypts Kubernetes Secrets at rest"
  enable_key_rotation     = true
  deletion_window_in_days = 30

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "AllowRootAdmin"
        Effect = "Allow"
        Principal = {
          AWS = "arn:aws:iam::${var.account_id}:root"
        }
        Action   = "kms:*"
        Resource = "*"
      },
      {
        Sid    = "AllowEKSClusterRole"
        Effect = "Allow"
        Principal = {
          AWS = aws_iam_role.eks_cluster.arn
        }
        Action = [
          "kms:Encrypt",
          "kms:Decrypt",
          "kms:ReEncrypt*",
          "kms:GenerateDataKey*",
          "kms:DescribeKey",
        ]
        Resource = "*"
      },
    ]
  })

  tags = {
    Name = "nitro-enc-svc-${var.environment}-eks-secrets"
  }
}

resource "aws_kms_alias" "eks_secrets" {
  name          = "alias/nitro-enc-svc/${var.environment}/eks-secrets"
  target_key_id = aws_kms_key.eks_secrets.key_id
}

# ── EBS key — encrypts node root volumes ──────────────────────────────────────

resource "aws_kms_key" "ebs" {
  description             = "nitro-enc-svc ${var.environment}: encrypts EKS node EBS volumes"
  enable_key_rotation     = true
  deletion_window_in_days = 30

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "AllowRootAdmin"
        Effect = "Allow"
        Principal = {
          AWS = "arn:aws:iam::${var.account_id}:root"
        }
        Action   = "kms:*"
        Resource = "*"
      },
      # EC2 Auto Scaling service role needs these to launch instances with encrypted EBS
      {
        Sid    = "AllowAutoScaling"
        Effect = "Allow"
        Principal = {
          AWS = "arn:aws:iam::${var.account_id}:role/aws-service-role/autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling"
        }
        Action = [
          "kms:Encrypt",
          "kms:Decrypt",
          "kms:ReEncrypt*",
          "kms:GenerateDataKey*",
          "kms:DescribeKey",
        ]
        Resource = "*"
      },
      {
        Sid    = "AllowAutoScalingGrants"
        Effect = "Allow"
        Principal = {
          AWS = "arn:aws:iam::${var.account_id}:role/aws-service-role/autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling"
        }
        Action   = "kms:CreateGrant"
        Resource = "*"
        Condition = {
          Bool = { "kms:GrantIsForAWSResource" = "true" }
        }
      },
    ]
  })

  tags = {
    Name = "nitro-enc-svc-${var.environment}-ebs"
  }
}

resource "aws_kms_alias" "ebs" {
  name          = "alias/nitro-enc-svc/${var.environment}/ebs"
  target_key_id = aws_kms_key.ebs.key_id
}

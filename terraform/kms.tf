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
      # Attestation-gated Decrypt — only added when PCR0 is provided
      var.kms_enclave_pcr0 != "" ? [
        {
          Sid    = "AllowEnclaveDecryptWithAttestation"
          Effect = "Allow"
          Principal = {
            AWS = aws_iam_role.enclave_node.arn
          }
          Action   = ["kms:Decrypt", "kms:DescribeKey"]
          Resource = "*"
          Condition = {
            StringEqualsIgnoreCase = {
              "kms:RecipientAttestation:PCR0" = var.kms_enclave_pcr0
            }
          }
        }
      ] : []
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

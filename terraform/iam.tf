# ── EKS Cluster role ──────────────────────────────────────────────────────────

resource "aws_iam_role" "eks_cluster" {
  name = "${var.cluster_name}-cluster"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "eks.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "eks_cluster_policy" {
  role       = aws_iam_role.eks_cluster.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEKSClusterPolicy"
}

# ── General node role (system / CoreDNS workloads) ────────────────────────────

resource "aws_iam_role" "general_node" {
  name = "${var.cluster_name}-general-node"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "general_node_worker" {
  role       = aws_iam_role.general_node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEKSWorkerNodePolicy"
}

resource "aws_iam_role_policy_attachment" "general_node_cni" {
  role       = aws_iam_role.general_node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEKS_CNI_Policy"
}

resource "aws_iam_role_policy_attachment" "general_node_ecr" {
  role       = aws_iam_role.general_node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEC2ContainerRegistryReadOnly"
}

resource "aws_iam_instance_profile" "general_node" {
  name = "${var.cluster_name}-general-node"
  role = aws_iam_role.general_node.name
}

# ── Enclave node role (Nitro Enclave runner nodes) ────────────────────────────

resource "aws_iam_role" "enclave_node" {
  name = "${var.cluster_name}-enclave-node"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "enclave_node_worker" {
  role       = aws_iam_role.enclave_node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEKSWorkerNodePolicy"
}

resource "aws_iam_role_policy_attachment" "enclave_node_ecr" {
  role       = aws_iam_role.enclave_node.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEC2ContainerRegistryReadOnly"
}

resource "aws_iam_role_policy" "enclave_kms" {
  name = "enclave-kms"
  role = aws_iam_role.enclave_node.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "kms:Decrypt",
        "kms:DescribeKey",
      ]
      Resource = aws_kms_key.enclave_dek.arn
    }]
  })
}

resource "aws_iam_role_policy" "enclave_secretsmanager" {
  name = "enclave-secretsmanager"
  role = aws_iam_role.enclave_node.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = "secretsmanager:GetSecretValue"
      Resource = aws_secretsmanager_secret.dek.arn
    }]
  })
}

resource "aws_iam_role_policy" "enclave_s3" {
  name = "enclave-s3"
  role = aws_iam_role.enclave_node.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = "s3:GetObject"
        Resource = "${aws_s3_bucket.schemas.arn}/${var.s3_prefix}*"
      },
      {
        Effect   = "Allow"
        Action   = "s3:ListBucket"
        Resource = aws_s3_bucket.schemas.arn
        Condition = {
          StringLike = { "s3:prefix" = "${var.s3_prefix}*" }
        }
      },
    ]
  })
}

resource "aws_iam_instance_profile" "enclave_node" {
  name = "${var.cluster_name}-enclave-node"
  role = aws_iam_role.enclave_node.name
}

# ── Pod Identity role — VPC CNI ───────────────────────────────────────────────

resource "aws_iam_role" "vpc_cni_pod_identity" {
  name = "${var.cluster_name}-vpc-cni-pod-identity"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "pods.eks.amazonaws.com" }
      Action    = ["sts:AssumeRole", "sts:TagSession"]
    }]
  })
}

resource "aws_iam_role_policy_attachment" "vpc_cni_pod_identity" {
  role       = aws_iam_role.vpc_cni_pod_identity.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonEKS_CNI_Policy"
}

# ── Pod Identity role — EBS CSI driver ───────────────────────────────────────

resource "aws_iam_role" "ebs_csi_pod_identity" {
  name = "${var.cluster_name}-ebs-csi-pod-identity"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "pods.eks.amazonaws.com" }
      Action    = ["sts:AssumeRole", "sts:TagSession"]
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ebs_csi_pod_identity" {
  role       = aws_iam_role.ebs_csi_pod_identity.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy"
}

resource "aws_iam_role_policy" "ebs_csi_kms" {
  name = "ebs-csi-kms"
  role = aws_iam_role.ebs_csi_pod_identity.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = "kms:*"
      Resource = aws_kms_key.ebs.arn
    }]
  })
}

# ── CodeBuild role ────────────────────────────────────────────────────────────

resource "aws_iam_role" "codebuild" {
  name = "${var.cluster_name}-codebuild"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "codebuild.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "codebuild_logs" {
  name = "codebuild-logs"
  role = aws_iam_role.codebuild.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "logs:CreateLogGroup",
        "logs:CreateLogStream",
        "logs:PutLogEvents",
      ]
      Resource = "arn:aws:logs:${var.aws_region}:${var.account_id}:log-group:/aws/codebuild/nitro-enc-svc-${var.environment}*"
    }]
  })
}

resource "aws_iam_role_policy" "codebuild_ecr" {
  name = "codebuild-ecr"
  role = aws_iam_role.codebuild.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = "ecr:GetAuthorizationToken"
        Resource = "*"
      },
      {
        Effect = "Allow"
        Action = [
          "ecr:BatchCheckLayerAvailability",
          "ecr:InitiateLayerUpload",
          "ecr:UploadLayerPart",
          "ecr:CompleteLayerUpload",
          "ecr:PutImage",
          "ecr:BatchGetImage",
          "ecr:GetDownloadUrlForLayer",
        ]
        Resource = [
          aws_ecr_repository.runner.arn,
          aws_ecr_repository.vsock_proxy.arn,
        ]
      },
    ]
  })
}

resource "aws_iam_role_policy" "codebuild_s3" {
  name = "codebuild-s3"
  role = aws_iam_role.codebuild.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = ["s3:GetObject", "s3:PutObject", "s3:GetObjectVersion"]
        Resource = [
          "${aws_s3_bucket.pipeline_artifacts.arn}/*",
        ]
      },
      {
        Effect   = "Allow"
        Action   = ["s3:GetObject", "s3:GetObjectVersion"]
        Resource = "${aws_s3_bucket.schemas.arn}/*"
      },
      {
        Effect   = "Allow"
        Action   = "s3:GetBucketLocation"
        Resource = [aws_s3_bucket.pipeline_artifacts.arn, aws_s3_bucket.schemas.arn]
      },
    ]
  })
}

resource "aws_iam_role_policy" "codebuild_kms" {
  name = "codebuild-kms"
  role = aws_iam_role.codebuild.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "kms:GenerateDataKey",
        "kms:Encrypt",
        "kms:DescribeKey",
        "kms:Decrypt",
      ]
      Resource = [
        aws_kms_key.enclave_dek.arn,
        aws_kms_key.ebs.arn,
      ]
    }]
  })
}

resource "aws_iam_role_policy" "codebuild_secretsmanager" {
  name = "codebuild-secretsmanager"
  role = aws_iam_role.codebuild.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Allow"
      Action   = "secretsmanager:GetSecretValue"
      Resource = aws_secretsmanager_secret.dek.arn
    }]
  })
}

# ── CodePipeline role ─────────────────────────────────────────────────────────

resource "aws_iam_role" "codepipeline" {
  name = "${var.cluster_name}-codepipeline"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "codepipeline.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "codepipeline_policy" {
  name = "codepipeline-policy"
  role = aws_iam_role.codepipeline.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "s3:GetObject",
          "s3:PutObject",
          "s3:GetBucketVersioning",
          "s3:GetObjectVersion",
          "s3:ListBucket",
        ]
        Resource = [
          aws_s3_bucket.pipeline_artifacts.arn,
          "${aws_s3_bucket.pipeline_artifacts.arn}/*",
        ]
      },
      {
        Effect = "Allow"
        Action = [
          "codebuild:StartBuild",
          "codebuild:BatchGetBuilds",
        ]
        Resource = aws_codebuild_project.build.arn
      },
      {
        Effect   = "Allow"
        Action   = "codestar-connections:UseConnection"
        Resource = var.codestar_connection_arn
      },
      {
        Effect = "Allow"
        Action = [
          "kms:Decrypt",
          "kms:GenerateDataKey",
        ]
        Resource = aws_kms_key.enclave_dek.arn
      },
    ]
  })
}
